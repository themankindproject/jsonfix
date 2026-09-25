//! Selecting which repair passes run.

use jsonfix::{Options, Repairs, repair_with};

#[test]
fn selected_repairs_are_enforced() {
    // Fences and comments only (trailing commas are always tolerated, like the
    // reference implementation).
    let opts = Options::all().with_repairs(Repairs::FENCES | Repairs::COMMENTS);
    assert_eq!(
        repair_with("```json\n{\"a\": 1} // done\n```", opts).unwrap(),
        "{\"a\": 1}"
    );
    assert!(repair_with("{a: 1}", opts).is_err());
    assert!(repair_with("{\"a\": 1", opts).is_err());
    assert!(repair_with("{'a': 1}", opts).is_err());

    // Truncation only.
    let opts = Options::all().with_repairs(Repairs::TRUNCATION);
    assert_eq!(repair_with("[1, 2", opts).unwrap(), "[1, 2]");
    assert!(repair_with("{'a': 1}", opts).is_err());

    // Numbers only.
    let opts = Options::all().with_repairs(Repairs::NUMBERS);
    assert_eq!(repair_with("{\"a\": .5}", opts).unwrap(), "{\"a\": 0.5}");
    assert!(repair_with("{'a': 1}", opts).is_err());

    // Unquoted keys and values only.
    let opts = Options::all().with_repairs(Repairs::UNQUOTED);
    assert_eq!(repair_with("{a: b}", opts).unwrap(), "{\"a\": \"b\"}");
    assert!(repair_with("[1, 2", opts).is_err());

    // Nothing at all behaves like strict parsing.
    let opts = Options::all().with_repairs(Repairs::NONE);
    assert!(repair_with("{\"a\": 1}", opts).is_ok());
    assert!(repair_with("{a: 1}", opts).is_err());
    assert!(repair_with("{\"a\": 1,}", opts).is_err());
}

#[test]
fn fences_alone_are_not_enough() {
    // A fenced block still needs truncation handling when it ends early.
    let opts = Options::all().with_repairs(Repairs::FENCES);
    assert!(repair_with("```json\n{\"a\": 1", opts).is_err());
    let opts = opts.with_repairs(Repairs::FENCES | Repairs::TRUNCATION);
    assert_eq!(
        repair_with("```json\n{\"a\": 1", opts).unwrap(),
        "{\"a\": 1}"
    );
}

#[test]
fn repair_flags_are_composable() {
    let allowed = Repairs::FENCES | Repairs::TRUNCATION;
    assert!(allowed.contains(Repairs::FENCES));
    assert!(allowed.contains(Repairs::TRUNCATION));
    assert!(!allowed.contains(Repairs::COMMENTS));
    assert_eq!(allowed.without(Repairs::FENCES), Repairs::TRUNCATION);
    assert_eq!(Repairs::NONE.union(Repairs::COMMENTS), Repairs::COMMENTS);
    assert_eq!(Repairs::ALL.bits().count_ones(), 12);
}

#[test]
fn allow_flags_are_composable() {
    use jsonfix::Allow;

    let allowed = Allow::OBJ | Allow::STR;
    assert!(allowed.contains(Allow::OBJ));
    assert!(!allowed.contains(Allow::ARR));
    assert_eq!(Allow::COLLECTION, Allow::OBJ | Allow::ARR);
    assert_eq!(Allow::ATOM, Allow::BOOL | Allow::NULL);
    assert!(Allow::ALL.contains(Allow::COLLECTION | Allow::ATOM | Allow::KEY));
    assert_eq!(Allow::ALL.bits().count_ones(), 7);
    assert!(Allow::NOTHING.is_empty());
}
