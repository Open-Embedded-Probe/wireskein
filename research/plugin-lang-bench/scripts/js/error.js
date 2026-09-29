function inner(frame) {
  var total = 0;
  // line 4 reads an undefined variable
  total = total + missingVariable.length;
  return total;
}
function outer() { return inner({}); }
outer();
