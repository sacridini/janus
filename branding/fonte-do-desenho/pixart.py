import numpy as np
from matplotlib.path import Path as MP
from janus import poly
import c2
SIL=MP(c2.sil); FR=MP(c2.faceR); FL=MP(c2.faceL)
EYR=MP(poly(c2.EYE)); EYL=MP(poly(c2.EYE)*[-1,1])
def grid(n, margin=1, gapdist=0.55, eyes=True, ss=3):
    """returns n x n array: 0 empty, 1 hair/beard, 2 face"""
    H=254.0; c=H/(n-2*margin); W=n*c
    x0=-W/2; y0=-margin*c
    A=np.zeros((n,n),int)
    fe=np.vstack([c2.faceR,c2.faceL])
    for r in range(n):
        for q in range(n):
            # supersample
            cnt={0:0,1:0,2:0}
            for a in range(ss):
                for b in range(ss):
                    x=x0+(q+(a+.5)/ss)*c; y=y0+(r+(b+.5)/ss)*c
                    if FR.contains_point((x,y)) or FL.contains_point((x,y)):
                        k=0 if eyes and (EYR.contains_point((x,y)) or EYL.contains_point((x,y))) else 2
                    elif SIL.contains_point((x,y)): k=1
                    else: k=0
                    cnt[k]+=1
            k=max(cnt,key=cnt.get)
            if k==1:
                x=x0+(q+.5)*c; y=y0+(r+.5)*c
                if np.min(np.hypot(fe[:,0]-x,fe[:,1]-y))<gapdist*c: k=0
            A[r,q]=k
    return A
def to_svg(A, hair, face, bg=None, gap=0.0, rx=0.0):
    n=A.shape[0]; out=[f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" width="{n}" height="{n}" shape-rendering="crispEdges">']
    if bg: out.append(f'<rect width="{n}" height="{n}" fill="{bg}"/>')
    for r in range(n):
        for q in range(n):
            if A[r,q]: out.append(f'<rect x="{q+gap/2}" y="{r+gap/2}" width="{1-gap}" height="{1-gap}" fill="{hair if A[r,q]==1 else face}"/>')
    return ''.join(out)+'</svg>'
if __name__=='__main__':
    h='<html><body style="margin:0;background:#0F0F0F;display:flex;gap:24px;padding:20px;align-items:flex-start;image-rendering:pixelated">'
    for n in (16,24,32,48):
        A=grid(n, eyes=n>=24)
        s=to_svg(A,'#4296FA','#4296FA')
        h+=f'<div style="width:{n*8}px">{s.replace(f"width=\"{n}\" height=\"{n}\"",f"width=\"{n*8}\" height=\"{n*8}\"")}</div>'
        h+=f'<div>{s}</div>'
    open('pa.html','w').write(h+'</body></html>')
def pattern(A, mode='rows', phase=0):
    B=A.copy(); n=A.shape[0]
    for r in range(n):
        for q in range(n):
            if A[r,q]==1:
                if mode=='rows' and (r+phase)%2: B[r,q]=0
                if mode=='dots' and ((r+phase)%2 or (q+n//2)%2==0): B[r,q]=0
    return B
