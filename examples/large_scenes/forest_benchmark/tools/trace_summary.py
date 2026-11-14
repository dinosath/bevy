"""Summarize a Bevy trace_chrome JSON (trace-*.json in cwd): average ms per span matching the given
substrings. Build the benchmark with `--features trace`.

Usage: python3 trace_summary.py extract_meshlet prepare_meshlet_per ..."""
import json,glob,collections,sys
f=sorted(glob.glob('trace-*.json'))[-1]
txt=open(f).read().rstrip().rstrip(',')
if not txt.endswith(']'): txt+=']'
ev=json.loads(txt)
dur=collections.defaultdict(float); cnt=collections.Counter(); stack={}
for e in ev:
    ph=e.get('ph')
    if ph=='X': dur[e['name']]+=e['dur']; cnt[e['name']]+=1
    elif ph=='B': stack.setdefault((e.get('pid'),e.get('tid')),[]).append(e)
    elif ph=='E':
        st=stack.get((e.get('pid'),e.get('tid')))
        if st: b=st.pop(); dur[b['name']]+=e['ts']-b['ts']; cnt[b['name']]+=1
want=sys.argv[1:]
for n,d in sorted(dur.items(),key=lambda x:-x[1]):
    if any(w in n for w in want): print(f"{n[:78]:78s} avg {d/cnt[n]/1000:8.3f} ms  calls {cnt[n]}")
