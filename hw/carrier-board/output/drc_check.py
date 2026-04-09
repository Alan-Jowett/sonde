import json, sys
from collections import Counter
d = json.load(open(sys.argv[1], encoding='utf-8'))
v = d.get('violations', [])
print(f'{len(v)} DRC violations')
for t, c in Counter(x.get('type','?') for x in v).most_common():
    print(f'  {c:3d}  {t}')
for x in v:
    if x['type'] in ('courtyards_overlap','shorting_items','items_not_allowed','clearance'):
        ds = [i.get('description','') for i in x.get('items',[])]
        print(f"  >> {x['type']}: {' | '.join(ds)}")
