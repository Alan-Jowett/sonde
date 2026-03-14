"""Resolve merge conflicts by keeping HEAD (current/ours) content."""
import sys

for path in sys.argv[1:]:
    with open(path, "r") as f:
        lines = f.readlines()

    result = []
    in_conflict = False
    keep = True
    for line in lines:
        stripped = line.rstrip("\n").rstrip("\r")
        if stripped.startswith("<<<<<<< "):
            in_conflict = True
            keep = True
            continue
        elif stripped == "=======" and in_conflict:
            keep = False
            continue
        elif stripped.startswith(">>>>>>> ") and in_conflict:
            in_conflict = False
            keep = True
            continue
        if keep:
            result.append(line)

    with open(path, "w") as f:
        f.writelines(result)

    with open(path, "r") as f:
        check = f.read()
    markers = sum(1 for l in check.split("\n")
                  if l.startswith("<<<<<<<") or l == "=======" or l.startswith(">>>>>>>"))
    print(f"{path}: {markers} markers remaining")
