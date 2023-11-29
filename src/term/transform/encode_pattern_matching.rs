use crate::term::{
  check::type_check::{DefinitionTypes, Type},
  Book, DefId, Name, Rule, RulePat, Term,
};

impl Book {
  pub fn encode_pattern_matching_functions(&mut self, def_types: &DefinitionTypes) {
    for def_id in self.defs.keys().copied().collect::<Vec<_>>() {
      let def_type = &def_types[&def_id];

      let is_matching_def = def_type.iter().any(|t| matches!(t, Type::Adt(_)));
      if is_matching_def {
        make_pattern_matching_def(self, def_id, def_type);
      } else {
        // For functions with only one rule that doesnt pattern match,
        // we just move the variables from arg to body.
        make_non_pattern_matching_def(self, def_id);
      }
    }
  }
}

/// For functions that don't pattern match, just move the arg variables into the body.
fn make_non_pattern_matching_def(book: &mut Book, def_id: DefId) {
  let def = book.defs.get_mut(&def_id).unwrap();
  let rule = def.rules.get_mut(0).unwrap();
  for pat in rule.pats.iter().rev() {
    let RulePat::Var(var) = pat else { unreachable!() };
    let bod = std::mem::replace(&mut rule.body, Term::Era);
    rule.body = Term::Lam { nam: Some(var.clone()), bod: Box::new(bod) };
  }
  rule.pats = vec![];
}

/// For function that do pattern match,
///  we break them into a tree of small matching functions
///  with the original rule bodies at the end.
fn make_pattern_matching_def(book: &mut Book, def_id: DefId, def_type: &[Type]) {
  let def_name = book.def_names.name(&def_id).unwrap().clone();
  let def = book.defs.get_mut(&def_id).unwrap();
  let crnt_rules = (0 .. def.rules.len()).collect();

  // First create a definition for each rule body
  let mut rule_bodies = vec![];
  for rule in def.rules.iter_mut() {
    let body = std::mem::replace(&mut rule.body, Term::Era);
    let body = make_rule_body(body, &rule.pats);
    rule_bodies.push(body);
  }
  for (rule_idx, body) in rule_bodies.into_iter().enumerate() {
    let rule_name = make_rule_name(&def_name, rule_idx);
    book.insert_def(rule_name, vec![Rule { pats: vec![], body }]);
  }

  // Generate scott-encoded pattern matching
  make_pattern_matching_case(book, def_type, def_id, &def_name, crnt_rules, vec![]);
}

fn make_rule_name(def_name: &Name, rule_idx: usize) -> Name {
  Name(format!("{def_name}$R{rule_idx}"))
}

fn make_rule_body(mut body: Term, pats: &[RulePat]) -> Term {
  // Add the lambdas for the pattern variables
  for pat in pats.iter().rev() {
    match pat {
      RulePat::Var(nam) => body = Term::Lam { nam: Some(nam.clone()), bod: Box::new(body) },
      RulePat::Ctr(_, vars) => {
        for var in vars.iter().rev() {
          let RulePat::Var(nam) = var else { unreachable!() };
          body = Term::Lam { nam: Some(nam.clone()), bod: Box::new(body) }
        }
      }
    }
  }
  body
}

fn make_pattern_matching_case(
  book: &mut Book,
  def_type: &[Type],
  def_id: DefId,
  crnt_name: &Name,
  crnt_rules: Vec<usize>,
  match_path: Vec<RulePat>,
) {
  let def = &book.defs[&def_id];
  // This is safe since we check exhaustiveness earlier.
  let fst_rule_idx = crnt_rules[0];
  let fst_rule = &def.rules[fst_rule_idx];
  let crnt_arg_idx = match_path.len();

  // Check if we've reached the end for this subfunction.
  // We did if all the (possibly zero) remaining patterns are variables and not matches.
  let all_args_done = crnt_arg_idx >= fst_rule.arity();
  let is_fst_rule_irrefutable =
    all_args_done || fst_rule.pats[crnt_arg_idx ..].iter().all(|p| matches!(p, RulePat::Var(_)));

  if is_fst_rule_irrefutable {
    // First rule will always be selected, generate leaf case.
    make_leaf_pattern_matching_case(book, def_id, crnt_name, fst_rule_idx, match_path);
  } else {
    let is_matching_case =
      crnt_rules.iter().any(|rule_idx| matches!(def.rules[*rule_idx].pats[crnt_arg_idx], RulePat::Ctr(..)));
    if is_matching_case {
      // Current arg is pattern matching, encode the pattern matching call
      make_branch_pattern_matching_case(book, def_type, def_id, crnt_name, crnt_rules, match_path);
    } else {
      // Current arg is not pattern matching, call next subfunction passing this arg.
      make_non_pattern_matching_case(book, def_type, def_id, crnt_name, crnt_rules, match_path);
    }
  }
}

/// Builds the function calling one of the original rule bodies.
fn make_leaf_pattern_matching_case(
  book: &mut Book,
  def_id: DefId,
  crnt_name: &Name,
  rule_idx: usize,
  match_path: Vec<RulePat>,
) {
  let def_name = book.def_names.name(&def_id).unwrap();
  let rule_def_name = make_rule_name(def_name, rule_idx);
  let rule_def_id = book.def_names.def_id(&rule_def_name).unwrap();
  let rule = &book.defs[&def_id].rules[rule_idx];

  // The term we're building
  let mut term = Term::Ref { def_id: rule_def_id };
  // Counts how many variables are used and then counts down to declare them.
  let mut matched_var_counter = 0;

  let use_var = |counter: &mut usize| {
    let nam = Name(format!("x{counter}"));
    *counter += 1;
    nam
  };
  let make_var = |counter: &mut usize| {
    *counter -= 1;
    Name(format!("x{counter}"))
  };
  let make_app = |term: Term, nam: Name| Term::App { fun: Box::new(term), arg: Box::new(Term::Var { nam }) };
  let make_lam = |nam: Name, term: Term| Term::Lam { nam: Some(nam), bod: Box::new(term) };

  // Add the applications to call the rule body
  term = match_path.iter().zip(&rule.pats).fold(term, |term, (matched, pat)| {
    match (matched, pat) {
      (RulePat::Var(_), RulePat::Var(_)) => make_app(term, use_var(&mut matched_var_counter)),
      (RulePat::Ctr(_, vars), RulePat::Ctr(_, _)) => {
        vars.iter().fold(term, |term, _| make_app(term, use_var(&mut matched_var_counter)))
      }
      // This particular rule was not matching on this arg but due to the other rules we had to match on a constructor.
      // So, to call the rule body we have to recreate the constructor.
      // (On scott encoding, if one of the cases is matched we must also match on all the other constructors for this arg)
      (RulePat::Ctr(ctr_nam, vars), RulePat::Var(_)) => {
        let ctr_ref_id = book.def_names.def_id(ctr_nam).unwrap();
        let ctr_args = vars.iter().map(|_| use_var(&mut matched_var_counter));
        let ctr_term = ctr_args.fold(Term::Ref { def_id: ctr_ref_id }, make_app);
        Term::App { fun: Box::new(term), arg: Box::new(ctr_term) }
      }
      (RulePat::Var(_), RulePat::Ctr(_, _)) => unreachable!(),
    }
  });

  // Add the lambdas to get the matched variables
  term = match_path.iter().rev().fold(term, |term, matched| match matched {
    RulePat::Var(_) => make_lam(make_var(&mut matched_var_counter), term),
    RulePat::Ctr(_, vars) => {
      vars.iter().fold(term, |term, _| make_lam(make_var(&mut matched_var_counter), term))
    }
  });

  add_case_to_book(book, crnt_name.clone(), term);
}

/// Builds a function for one of the pattern matches of the original one, as well as the next subfunctions recursively.
fn make_branch_pattern_matching_case(
  book: &mut Book,
  def_type: &[Type],
  def_id: DefId,
  crnt_name: &Name,
  crnt_rules: Vec<usize>,
  match_path: Vec<RulePat>,
) {
  fn filter_rules(def_rules: &[Rule], crnt_rules: &[usize], arg_idx: usize, ctr: &Name) -> Vec<usize> {
    crnt_rules
      .iter()
      .copied()
      .filter(|&rule_idx| match &def_rules[rule_idx].pats[arg_idx] {
        RulePat::Var(_) => true,
        RulePat::Ctr(nam, _) => nam == ctr,
      })
      .collect()
  }
  let make_next_fn_name = |crnt_name, ctr_name| Name(format!("{crnt_name}$P{ctr_name}"));
  let make_app = |term, arg| Term::App { fun: Box::new(term), arg: Box::new(arg) };
  let make_lam = |nam, term| Term::Lam { nam: Some(nam), bod: Box::new(term) };
  let use_var = |counter: &mut usize| {
    let nam = Name(format!("x{counter}"));
    *counter += 1;
    nam
  };
  let make_var = |counter: &mut usize| {
    *counter -= 1;
    Name(format!("x{counter}"))
  };

  let crnt_arg_idx = match_path.len();
  let Type::Adt(next_type) = &def_type[crnt_arg_idx] else { unreachable!() };
  let next_ctrs = book.adts[next_type].ctrs.clone();

  // First we create the subfunctions
  // TODO: We could group together functions with same arity that map to the same (default) case.
  for (next_ctr, &next_ctr_ari) in next_ctrs.iter() {
    let def = &book.defs[&def_id];
    let crnt_name = make_next_fn_name(crnt_name, next_ctr);
    let crnt_rules = filter_rules(&def.rules, &crnt_rules, match_path.len(), next_ctr);
    let new_vars = RulePat::Ctr(next_ctr.clone(), vec![RulePat::Var(Name::new("")); next_ctr_ari]);
    let mut match_path = match_path.clone();
    match_path.push(new_vars);
    make_pattern_matching_case(book, def_type, def_id, &crnt_name, crnt_rules, match_path);
  }

  // Pattern matched value
  let term = Term::Var { nam: Name::new("x") };
  let term = next_ctrs.keys().fold(term, |term, ctr| {
    let name = make_next_fn_name(crnt_name, ctr);
    let def_id = book.def_names.def_id(&name).unwrap();
    make_app(term, Term::Ref { def_id })
  });

  let mut var_count = 0;

  // Applied arguments
  let term = match_path.iter().fold(term, |term, pat| match pat {
    RulePat::Var(_) => make_app(term, Term::Var { nam: use_var(&mut var_count) }),
    RulePat::Ctr(_, vars) => {
      vars.iter().fold(term, |term, _| make_app(term, Term::Var { nam: use_var(&mut var_count) }))
    }
  });

  // Lambdas for arguments
  let term = match_path.iter().rev().fold(term, |term, pat| match pat {
    RulePat::Var(_) => make_lam(make_var(&mut var_count), term),
    RulePat::Ctr(_, vars) => vars.iter().fold(term, |term, _| make_lam(make_var(&mut var_count), term)),
  });

  // Lambda for the matched variable
  let term = Term::Lam { nam: Some(Name::new("x")), bod: Box::new(term) };

  add_case_to_book(book, crnt_name.clone(), term);
}

fn make_non_pattern_matching_case(
  book: &mut Book,
  def_type: &[Type],
  def_id: DefId,
  crnt_name: &Name,
  crnt_rules: Vec<usize>,
  mut match_path: Vec<RulePat>,
) {
  let arg_name = Name::new("x");
  let nxt_name = Name::new("nxt");
  let nxt_def_name = Name(format!("{crnt_name}$P"));

  // Make next function
  match_path.push(RulePat::Var(arg_name.clone()));
  make_pattern_matching_case(book, def_type, def_id, &nxt_def_name, crnt_rules, match_path);

  // Make call to next function
  let nxt_def_id = book.def_names.def_id(&nxt_def_name).unwrap();
  let term = Term::Lam {
    nam: Some(arg_name.clone()),
    bod: Box::new(Term::Lam {
      nam: Some(nxt_name.clone()),
      bod: Box::new(Term::App {
        fun: Box::new(Term::App {
          fun: Box::new(Term::Ref { def_id: nxt_def_id }),
          arg: Box::new(Term::Var { nam: nxt_name }),
        }),
        arg: Box::new(Term::Var { nam: arg_name }),
      }),
    }),
  };
  add_case_to_book(book, crnt_name.clone(), term);
}

fn add_case_to_book(book: &mut Book, nam: Name, body: Term) {
  if let Some(def_id) = book.def_names.def_id(&nam) {
    book.defs.get_mut(&def_id).unwrap().rules = vec![Rule { pats: vec![], body }];
  } else {
    book.insert_def(nam, vec![Rule { pats: vec![], body }]);
  }
}
