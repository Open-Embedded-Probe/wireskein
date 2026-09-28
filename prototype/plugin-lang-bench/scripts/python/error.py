def inner(frame):
    total = 0
    # line 4 reads an undefined variable
    total = total + missing_variable.length
    return total
def outer(): return inner({})
outer()
