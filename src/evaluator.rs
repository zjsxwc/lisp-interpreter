use super::*;
use std::collections::HashMap;
use std::iter;

#[derive(Debug, Clone)]
pub struct State {
    bound: HashMap<String, LispValue>,
}

impl State {
    pub fn new() -> State {
        State {
            bound: HashMap::new(),
        }
    }

    pub fn get_variable_value(&self, var_name: &str) -> Option<LispValue> {
        self.bound.get(var_name).cloned()
    }

    pub fn set_variable(&mut self, var_name: &str, val: LispValue) {
        self.bound.insert(var_name.into(), val);
    }
}

// TODO: remove superfluous derives
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Instr {
    // Function and argument vector, Tail call
    EvalFunction(Vec<LispExpr>, bool),
    PopAndSet(String),
    PopState,
    // Function, Argument Count, Tail call
    EvalFunctionEager(LispFunc, usize, bool),

    // Removes top n instructions from stack
    PopInstructions(usize),
    // Pops value from stack - removes top n instructions if true
    CondPopInstructions(usize),
    PushValue(LispValue),
    CloneValue(usize),
    PushVariable(String),
}

fn unitary_int<F: Fn(u64) -> Result<LispValue, EvaluationError>>(
    stack: &mut Vec<LispValue>,
    f: F,
) -> Result<(), EvaluationError> {
    match stack.pop().unwrap() {
        LispValue::Integer(i) => Ok(stack.push(f(i)?)),
        _ => Err(EvaluationError::ArgumentTypeMismatch),
    }
}

fn unitary_list<F: Fn(Vec<LispValue>) -> Result<LispValue, EvaluationError>>(
    stack: &mut Vec<LispValue>,
    f: F,
) -> Result<(), EvaluationError> {
    match stack.pop().unwrap() {
        LispValue::SubValue(v) => Ok(stack.push(f(v)?)),
        _ => Err(EvaluationError::ArgumentTypeMismatch),
    }
}

macro_rules! destructure {
    ( $y:ident, $x:expr ) => {{$x}};

    ( $y:ident, [ $( $i:ident ),* ], $body:expr ) => {
        {
            if let ($( Some($i), )* None) = {
                let mut iter = $y.into_iter();
                ( $( destructure!($i, iter.next()), )* iter.next() )
            } {
                Ok($body)
            } else {
                Err(EvaluationError::ArgumentCountMismatch)
            }
        }
    };
}

// TODO: check that only top level matters for determining whether we
//       are dealing with a closure
pub fn compile_expr(
    expr: LispExpr,
    stack: &[LispValue],
    state: &State,
) -> Result<(bool, Vec<Instr>), EvaluationError> {
    let mut vek = vec![];
    let mut is_closure = false;

    match expr {
        LispExpr::Argument(offset) => {
            vek.push(Instr::CloneValue(offset));
        }
        LispExpr::Value(v) => {
            vek.push(Instr::PushValue(v));
        }
        LispExpr::OpVar(n) => {
            vek.push(Instr::PushVariable(n));
        }
        LispExpr::Macro(..) => {
            // FIXME: this is reachable - just return an error
            unreachable!()
        }
        LispExpr::Call(mut expr_list, is_tail_call) => {
            // step 1: remove head expression
            let head_expr = expr_list.remove(0);

            // step 2: queue function evaluation with tail
            match head_expr {
                LispExpr::Macro(LispMacro::Cond) => {
                    destructure!(expr_list, [boolean, true_expr, false_expr], {
                        let true_expr_vec = compile_expr(true_expr, stack, state)?.1;
                        let false_expr_vec = compile_expr(false_expr, stack, state)?.1;
                        let true_expr_len = true_expr_vec.len();
                        let false_expr_len = false_expr_vec.len();

                        vek.extend(true_expr_vec);
                        vek.push(Instr::PopInstructions(true_expr_len));
                        vek.extend(false_expr_vec);
                        vek.push(Instr::CondPopInstructions(false_expr_len + 1));
                        vek.extend(compile_expr(boolean, stack, state)?.1);
                    })?
                }
                LispExpr::Macro(LispMacro::Lambda) => {
                    destructure!(
                        expr_list,
                        [arg_list, body],
                        if let LispExpr::Call(arg_vec, _is_tail_call) = arg_list {
                            let args = arg_vec
                                .into_iter()
                                .map(|expr| match expr {
                                    LispExpr::OpVar(name) => Ok(name),
                                    _ => Err(EvaluationError::MalformedDefinition),
                                })
                                .collect::<Result<Vec<_>, _>>()?;

                            // If there are any references to function arguments in
                            // the lambda body, we should resolve them before
                            // creating the lambda.
                            // This enables us to do closures.
                            let (replaced_args, walked_body) = body.replace_args(stack);
                            is_closure = is_closure || replaced_args;

                            let f = LispFunc::new_custom(args, walked_body, state);

                            vek.push(Instr::PushValue(LispValue::Function(f)));
                        } else {
                            return Err(EvaluationError::ArgumentTypeMismatch);
                        }
                    )?
                }
                LispExpr::Macro(LispMacro::Define) => destructure!(
                    expr_list,
                    [var_name, definition],
                    if let LispExpr::OpVar(name) = var_name {
                        vek.push(Instr::PopAndSet(name));
                        vek.extend(compile_expr(definition, stack, state)?.1);
                    } else {
                        return Err(EvaluationError::ArgumentTypeMismatch);
                    }
                )?,
                _ => {
                    vek.push(Instr::EvalFunction(expr_list, is_tail_call));

                    // step 3: queue evaluation of head
                    vek.extend(compile_expr(head_expr, stack, state)?.1);
                } 
            }
        }
    }

    Ok((is_closure, vek))
}

pub fn eval<'e>(expr: &'e LispExpr, state: &mut State) -> Result<LispValue, EvaluationError> {
    let mut return_values: Vec<LispValue> = Vec::new();
    let mut instructions = compile_expr(expr.clone(), &return_values[..], state)?.1;
    let mut stack_pointers = vec![];
    let mut current_stack = 0;

    while let Some(instr) = instructions.pop() {
        // println!("stack len: {}, instruction: {:?}", return_values.len(), instr);

        match instr {
            Instr::PopInstructions(n) => {
                let new_len = instructions.len() - n;
                instructions.truncate(new_len);
            }
            Instr::CondPopInstructions(n) => {
                if let LispValue::Boolean(b) = return_values.pop().unwrap() {
                    if b {
                        instructions.push(Instr::PopInstructions(n));
                    }
                } else {
                    return Err(EvaluationError::ArgumentTypeMismatch);
                }
            }
            Instr::PushValue(v) => {
                return_values.push(v);
            }
            Instr::CloneValue(offset) => {
                let index = current_stack + offset;
                let value = return_values[index].clone();
                return_values.push(value);
            }
            Instr::PushVariable(n) => if let Some(v) = state.get_variable_value(&n) {
                return_values.push(v);
            } else {
                return Err(EvaluationError::UnknownVariable(n));
            },

            Instr::PopState => {
                let val = return_values.pop().unwrap();

                return_values.truncate(current_stack);
                return_values.push(val);

                current_stack = stack_pointers.pop().unwrap();
            }
            // Pops a function off the value stack and applies it
            Instr::EvalFunction(expr_list, is_tail_call) => {
                if let LispValue::Function(f) = return_values.pop().unwrap() {
                    // Eager argument evaluation: evaluate all arguments before
                    // calling the function.
                    instructions.push(Instr::EvalFunctionEager(f, expr_list.len(), is_tail_call));

                    for expr in expr_list.into_iter().rev() {
                        let instr_vec = compile_expr(expr, &return_values[current_stack..], state)?.1;
                        instructions.extend(instr_vec);
                    }
                } else {
                    return Err(EvaluationError::NonFunctionApplication);
                }
            }
            Instr::EvalFunctionEager(func, arg_count, is_tail_call) => {
                match func {
                    LispFunc::BuiltIn(BuiltIn::List) => {
                        let len = return_values.len();
                        let new_vec = return_values.split_off(len - arg_count);
                        return_values.push(LispValue::SubValue(new_vec));
                    }
                    LispFunc::BuiltIn(BuiltIn::Car) => if arg_count == 1 {
                        unitary_list(&mut return_values, |mut vec| match vec.pop() {
                            Some(car) => Ok(car),
                            None => Err(EvaluationError::EmptyList),
                        })?
                    } else {
                        return Err(EvaluationError::ArgumentCountMismatch);
                    },
                    LispFunc::BuiltIn(BuiltIn::Cdr) => if arg_count == 1 {
                        unitary_list(&mut return_values, |mut vec| match vec.pop() {
                            Some(_) => Ok(LispValue::SubValue(vec)),
                            None => Err(EvaluationError::EmptyList),
                        })?
                    } else {
                        return Err(EvaluationError::ArgumentCountMismatch);
                    },
                    LispFunc::BuiltIn(BuiltIn::CheckNull) => if arg_count == 1 {
                        unitary_list(&mut return_values, |vec| {
                            Ok(LispValue::Boolean(vec.is_empty()))
                        })?
                    } else {
                        return Err(EvaluationError::ArgumentCountMismatch);
                    },
                    LispFunc::BuiltIn(BuiltIn::AddOne) => if arg_count == 1 {                        
                        unitary_int(&mut return_values, |i| Ok(LispValue::Integer(i + 1)))?
                    } else {
                        return Err(EvaluationError::ArgumentCountMismatch);
                    },
                    LispFunc::BuiltIn(BuiltIn::SubOne) => if arg_count == 1 {
                        unitary_int(&mut return_values, |i| if i > 0 {
                            Ok(LispValue::Integer(i - 1))
                        } else {
                            Err(EvaluationError::SubZero)
                        })?
                    } else {
                        return Err(EvaluationError::ArgumentCountMismatch);
                    },
                    LispFunc::BuiltIn(BuiltIn::Cons) => if arg_count == 2 {
                        if let LispValue::SubValue(mut new_vec) = return_values.pop().unwrap() {
                            new_vec.push(return_values.pop().unwrap());
                            return_values.push(LispValue::SubValue(new_vec));
                        } else {
                            return Err(EvaluationError::ArgumentTypeMismatch);
                        }
                    } else {
                        return Err(EvaluationError::ArgumentCountMismatch);
                    },
                    LispFunc::BuiltIn(BuiltIn::CheckZero) => if arg_count == 1 {
                        unitary_int(&mut return_values, |i| Ok(LispValue::Boolean(i == 0)))?
                    } else {
                        return Err(EvaluationError::ArgumentCountMismatch);
                    },
                    // LispFunc::BuiltIn(n) => return Err(EvaluationError::UnknownVariable(n.to_string())),
                    LispFunc::Custom(mut f) => {
                        // Too many arguments or none at all.
                        if f.arg_count < arg_count || arg_count == 0 {
                            return Err(EvaluationError::ArgumentCountMismatch);
                        }
                        // Not enough arguments, let's create a lambda that takes
                        // the remainder.
                        else if arg_count < f.arg_count {
                            stack_pointers.push(current_stack);
                            current_stack = return_values.len() - arg_count;

                            let funk_arg_count = f.arg_count;
                            let continuation = LispFunc::create_continuation(
                                LispFunc::Custom(f),
                                funk_arg_count,
                                arg_count,
                                &return_values[current_stack..],
                            );

                            instructions.push(Instr::PopState);
                            return_values.truncate(current_stack);
                            return_values.push(LispValue::Function(continuation));
                        }
                        // Exactly right number of arguments. Let's evaluate.
                        else if is_tail_call {
                            // println!("doing tail call");

                            // Remove old arguments of the stack.
                            let top_index = return_values.len() - arg_count;
                            return_values.splice(current_stack..top_index, iter::empty());
                            let byte_code = f.compile(&return_values[current_stack..], state)?;

                            instructions.extend(byte_code);
                        } else {
                            stack_pointers.push(current_stack);
                            current_stack = return_values.len() - arg_count;
                            let byte_code = f.compile(&return_values[current_stack..], state)?;

                            instructions.push(Instr::PopState);
                            instructions.extend(byte_code);
                        }
                    }
                }
            }
            Instr::PopAndSet(var_name) => {
                state.set_variable(&var_name, return_values.pop().unwrap());
                return_values.push(LispValue::SubValue(Vec::new()));
            }
        }
    }

    assert!(stack_pointers.is_empty());
    assert_eq!(current_stack, 0);
    assert!(instructions.is_empty());
    assert_eq!(return_values.len(), 1);
    Ok(return_values.pop().unwrap())
}
