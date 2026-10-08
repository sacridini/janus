import math, numpy as np
from bf import blackfigure
from textpath import text_d
TER='#C2683A'; BLK='#17140F'; BONE='#ECE5D6'
def meander(x0,y0,w,g=4.0,color='#000',lw=1.6,pid='j'):
    n=int(w//(6*g)); w2=n*6*g; xs=x0+(w-w2)/2
    d=[f'M{x0},{y0} H{x0+w}', f'M{x0},{y0+6*g} H{x0+w}']
    for i in range(n):
        x=xs+i*6*g+g
        d.append(f'M{x:.2f},{y0+6*g:.2f} V{y0+g:.2f} H{x+4*g:.2f} V{y0+4.2*g:.2f} H{x+2*g:.2f} V{y0+2.6*g:.2f}')
    return f'<path id="{pid}-meandro" d="{" ".join(d)}" fill="none" stroke="{color}" stroke-width="{lw}" stroke-linecap="square"/>'
def classic_word(color, size=100, track=None, x0=0, y0=0, font='pagella', text='JANUS', pid='j'):
    track=size*0.16 if track is None else track
    d,w=text_d(text,font,size,track=track,x0=x0,y0=y0)
    return f'<path id="{pid}-nome" d="{d}" fill="{color}"/>', w
def head(ink,ground,pid='j',knockout=True,**kw): return blackfigure(ink,ground,knockout=knockout,pid=pid,**kw)
def lockup_classic_v(ink, ground, word=True, pid='v', knockout=True):
    out=f'<g id="{pid}-cabeca">{head(ink,ground,pid,knockout)}</g>'
    out+=meander(-115,312,230,g=3.6,color=ink,lw=1.8,pid=pid)
    if word:
        wd,w=classic_word(ink,size=64,pid=pid)
        out+=f'<g transform="translate({-w/2:.2f},402)">{wd}</g>'
    return out, f'-140 -40 280 {470 if word else 380}'
def lockup_classic_h(ink, ground, pid='h', knockout=True):
    wd,w=classic_word(ink,size=92,pid=pid)
    out=f'<g id="{pid}-cabeca">{head(ink,ground,pid,knockout)}</g><g transform="translate(150,178)">{wd}</g>'
    return out, f'-112 -30 {150+w+124} 340'
def mark(ink,ground,pid='m',knockout=True):
    return f'<g id="{pid}-cabeca">{head(ink,ground,pid,knockout)}</g>', '-112 -26 224 330'
def coin(ink, ground, field, pid='c', knockout=True, detail='full', beads=True, k=0.80):
    r=170; cy=128
    b=''.join(f'<circle cx="{(r-6)*math.cos(a):.2f}" cy="{cy+(r-6)*math.sin(a):.2f}" r="3.1"/>' for a in np.linspace(0,2*math.pi,64,endpoint=False)) if beads else ''
    ring=f'<circle cx="0" cy="{cy}" r="{r-16}" fill="none" stroke="{ink}" stroke-width="1.6"/>' if beads else ''
    lw=1.9 if detail=='full' else 5.0
    return (f'<circle id="{pid}-campo" cx="0" cy="{cy}" r="{r}" fill="{field}"/><g id="{pid}-perolas" fill="{ink}">{b}</g>{ring}'
            f'<g id="{pid}-cabeca" transform="translate(0,{cy-k*143:.2f}) scale({k})">{head(ink,ground,pid,knockout,detail=detail,lw=lw)}</g>'), f'-{r} {cy-r} {2*r} {2*r}'
