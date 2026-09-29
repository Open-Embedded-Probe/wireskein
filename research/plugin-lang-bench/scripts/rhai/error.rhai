fn inner(frame) {
    let total = 0;
    // line 4 reads an undefined variable
    total = total + missing_variable.len();
    total
}
fn outer() { inner(#{}) }
outer();
