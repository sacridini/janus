import numpy as np, math
def cubic(p0,p1,p2,p3,n=28):
    t=np.linspace(0,1,n)[:,None]; p0,p1,p2,p3=map(np.array,(p0,p1,p2,p3))
    return ((1-t)**3*p0+3*(1-t)**2*t*p1+3*(1-t)*t**2*p2+t**3*p3)

def poly(segs):
    pts=[];cur=None
    for s in segs:
        if s[0]=='M': cur=s[1]; pts.append(cur)
        elif s[0]=='L': pts.append(s[1]); cur=s[1]
        else:
            c=cubic(cur,*s[1:]); pts+=list(map(tuple,c[1:])); cur=s[3]
    return np.array(pts,float)

def d_of(segs, mirror=False, close=False):
    f=(lambda p:f"{-p[0]:.2f},{p[1]:.2f}") if mirror else (lambda p:f"{p[0]:.2f},{p[1]:.2f}")
    o=[]
    for s in segs:
        o.append(s[0]+' '.join(f(q) for q in s[1:]))
    return ' '.join(o)+(' Z' if close else '')

def reverse(segs):
    nodes=[];cur=None
    for s in segs:
        if s[0]=='M': cur=s[1]; continue
        nodes.append((s,cur)); cur=s[-1]
    out=[('M',cur)]
    for s,start in reversed(nodes):
        if s[0]=='L': out.append(('L',start))
        else: out.append(('C',s[2],s[1],start))
    return out

def mirror(segs): return [(s[0],)+tuple((-q[0],q[1]) for q in s[1:]) for s in segs]

def hspans(P, y):
    """x-intervals where horizontal line y is inside closed polygon P"""
    xs=[]
    n=len(P)
    for i in range(n):
        (x1,y1),(x2,y2)=P[i],P[(i+1)%n]
        if (y1<=y<y2) or (y2<=y<y1):
            xs.append(x1+(y-y1)*(x2-x1)/(y2-y1))
    xs.sort()
    return [(xs[i],xs[i+1]) for i in range(0,len(xs)-1,2)]

def subtract(spans, cuts):
    out=[]
    for a,b in spans:
        segs=[(a,b)]
        for c,d in cuts:
            nxt=[]
            for u,v in segs:
                if d<=u or c>=v: nxt.append((u,v)); continue
                if c>u: nxt.append((u,c))
                if d<v: nxt.append((d,v))
            segs=nxt
        out+=segs
    return out
