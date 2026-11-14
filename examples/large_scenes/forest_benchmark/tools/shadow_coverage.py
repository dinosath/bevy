"""Fraction of ground pixels that are shadowed, relative to the first image (the reference).

Usage: python3 shadow_coverage.py ref.png other.png ...  (crops the lower part of a 1280x720 frame;
meaningful for the aerial path of forest_benchmark). A proxy for shadow quality: coarse shadow LOD
makes thin-geometry shadows disappear."""
import sys
from PIL import Image
def lum(p): return 0.2126*p[0]+0.7152*p[1]+0.0722*p[2]
def crop(n): return Image.open(n).convert('RGB').crop((0,300,1280,720))
ref=crop(sys.argv[1]); g=sorted(lum(p) for p in ref.getdata() if p[1]>=p[0] and p[1]>=p[2]); thr=0.7*g[len(g)//2]
for n in sys.argv[1:]:
    t=s=0
    for p in crop(n).getdata():
        if p[1]>=p[0] and p[1]>=p[2]: t+=1; s+=lum(p)<thr
    print(f"{n:14s} shadowed-ground fraction {100*s/t:5.2f}%")
