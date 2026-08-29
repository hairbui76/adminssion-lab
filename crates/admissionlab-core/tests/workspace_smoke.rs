#[test]
fn core_crate_is_linkable() {
    assert_eq!(admissionlab_core::crate_identity(), "admissionlab-core");
}
