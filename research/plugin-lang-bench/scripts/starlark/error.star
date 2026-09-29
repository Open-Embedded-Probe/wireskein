def inner(frame):
    total = 0
    # line 4 reads a missing field of a dict (undefined names are compile-time errors in Starlark)
    total = total + frame.missing_field
    return total

def outer():
    return inner({})

outer()
