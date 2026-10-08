import math, numpy as np
from janus import hspans, subtract, d_of
import c2
from c2 import face, EYE, sil, faceR, faceL
def R(x,y,w,h,fill=None):
    f=f' fill="{fill}"' if fill else ''
    return f'<rect x="{x:.2f}" y="{y:.2f}" width="{w:.2f}" height="{h:.2f}" rx="{h/2:.2f}"{f}/>'
def faces_d():
    return (d_of(face,close=True)+' '+d_of(EYE,close=True), d_of(face,mirror=True,close=True)+' '+d_of(EYE,mirror=True,close=True))
def faces_svg(fg):
    a,b=faces_d(); return f'<path d="{a}" fill="{fg}" fill-rule="evenodd"/><path d="{b}" fill="{fg}" fill-rule="evenodd"/>'
def rows(period=7.0, th=4.2, thfn=None):
    """list of (y, [(a,b),...]) stripe spans outside the faces"""
    out=[]; y=period/2
    while y<256:
        sp=hspans(sil,y)
        cuts=[(a-2.2,b+2.2) for F in (faceR,faceL) for a,b in hspans(F,y)]
        out.append((y, [(a,b) for a,b in subtract(sp,cuts) if b-a>1], thfn(y) if thfn else th))
        y+=period
    return out
def v_base(fg, accent=None, period=7, th=4.2):
    return ''.join(R(a,y-t/2,b-a,t) for y,sp,t in rows(period,th) for a,b in sp).join([f'<g fill="{fg}">','</g>'])+faces_svg(fg)
def v_split(fg, accent=None, gap=4.0):
    out=[]
    for y,sp,t in rows():
        for a,b in sp:
            if a<0<b: out+= [R(a,y-t/2,-gap/2-a,t), R(gap/2,y-t/2,b-gap/2,t)]
            else: out.append(R(a,y-t/2,b-a,t))
    return f'<g fill="{fg}">{"".join(out)}</g>'+faces_svg(fg)
def v_now(fg, accent, extend=False):
    out=[]; rs=rows()
    k=min(range(len(rs)),key=lambda i:abs(rs[i][0]-112))
    for i,(y,sp,t) in enumerate(rs):
        for a,b in sp:
            out.append(R(a,y-t/2,b-a,t, accent if i==k else None))
    extra=''
    if extend:
        y=rs[k][0]; t=rs[k][2]
        extra=R(-128,y-t/2,256,t,accent)
        out=[o for i,o in enumerate(out)]
    return f'<g fill="{fg}">{"".join(out)}</g>'+faces_svg(fg)+extra
def v_grad(fg, accent=None):
    rs=rows(6.4, thfn=lambda y: 1.6+3.6*(y/254))
    return f'<g fill="{fg}">'+''.join(R(a,y-t/2,b-a,t) for y,sp,t in rs for a,b in sp)+'</g>'+faces_svg(fg)
def v_fine(fg, accent=None):
    rs=rows(4.6,2.2)
    return f'<g fill="{fg}">'+''.join(R(a,y-t/2,b-a,t) for y,sp,t in rs for a,b in sp)+'</g>'+faces_svg(fg)
def v_pixel(fg, accent=None, cell=7.0, gapx=1.8, h=5.2):
    """hair and beard as a grid of square cells (symmetric about x=0), faces solid"""
    out=[]; w=cell-gapx
    for y,sp,t in rows(cell,h):
        for a,b in sp:
            for i in range(-20,20):
                x=i*cell+gapx/2
                if x>=a-0.4 and x+w<=b+0.4:
                    out.append(f'<rect x="{x:.2f}" y="{y-h/2:.2f}" width="{w:.2f}" height="{h:.2f}" rx="0.8"/>')
    return f'<g fill="{fg}">{"".join(out)}</g>'+faces_svg(fg)
def v_inverse(fg, accent=None, uid='inv'):
    # solid hair/beard, striped faces
    sild=c2.d_of(c2.PROF)+' '+' '.join(c2.d_of(c2.reverse(c2.mirror(c2.PROF))).split(' ')[1:])+' Z'
    a,b=faces_d()
    fa=d_of(face,close=True); fb=d_of(face,mirror=True,close=True)
    stripes=''.join(R(-100,y-1.6,200,3.2) for y in np.arange(3.5,256,5.2))
    eyes=d_of(EYE,close=True)+' '+d_of(EYE,mirror=True,close=True)
    return (f'<defs><clipPath id="{uid}"><path d="{fa} {fb}"/></clipPath>'
            f'<mask id="{uid}m" maskUnits="userSpaceOnUse" x="-200" y="-50" width="400" height="400"><rect x="-200" y="-50" width="400" height="400" fill="#fff"/>'
            f'<path d="{fa} {fb}" fill="#000" stroke="#000" stroke-width="4.4"/></mask></defs>'
            f'<path d="{sild}" fill="{fg}" mask="url(#{uid}m)"/>'
            f'<g clip-path="url(#{uid})" fill="{fg}">{stripes}</g>'
            f'<path d="{eyes}" fill="{fg}"/>')
def v_small(fg, accent=None):
    rs=rows(16,9.6)
    return f'<g fill="{fg}">'+''.join(R(a,y-t/2,b-a,t) for y,sp,t in rs for a,b in sp)+'</g>'+faces_svg(fg)
def v_split_line(fg, accent, gap=4.0):
    rs=rows(); k=min(range(len(rs)),key=lambda i:abs(rs[i][0]-112)); y,_,t=rs[k]
    return v_split(fg,gap=gap)+R(-128,y-t/2,256,t,accent)
