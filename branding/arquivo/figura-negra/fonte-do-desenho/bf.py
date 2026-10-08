# "black-figure" classical Janus: solid silhouette, details incised in the ground colour
import math, numpy as np
from matplotlib.path import Path as MPath
from janus import cubic, poly, d_of, reverse, mirror
PROF=[('M',(0,4)),
 ('C',(36,0),(64,9),(75,36)),
 ('C',(79,47),(80,59),(78,68)),
 ('C',(80,78),(82,88),(82,96)),
 ('C',(83.5,98),(84.5,100),(84,102)),
 ('C',(83.5,104),(82.5,105),(82.5,107)),
 ('C',(88,116),(96,128),(100,137)),
 ('C',(103,142),(100.5,147),(95,146.5)),
 ('L',(88,147)),
 ('C',(92,151),(92.5,155),(89,157.5)),
 ('L',(84.5,159.5)),
 ('C',(88.5,160.5),(89.5,163.5),(87,166.5)),
 ('C',(91,178),(92,194),(88,210)),
 ('C',(85,230),(72,248),(54,258)),
 ('C',(40,265),(20,266),(0,262))]
NECK='M-31,250 L31,250 L33,288 C22,294 10,296 0,296 C-10,296 -22,294 -33,288 Z'
FACE=[('M',(78,68))]+PROF[3:9]+[
 ('C',(80,150),(72,152),(64,150)),
 ('C',(52,147),(44,141),(41,130)),
 ('C',(38,118),(37,104),(40,92)),
 ('C',(44,78),(58,70),(78,68))]
SIL=np.vstack([poly(PROF), poly(reverse(mirror(PROF)))[1:]])
FP=poly(FACE)
def P(p,sx): return f'{sx*p[0]:.2f},{p[1]:.2f}'
def pl(pts,sx=1): return 'M'+' L'.join(P(p,sx) for p in pts)
def leaf(cx,cy,ang,L=17,W=6.2):
    ca,sa=math.cos(ang),math.sin(ang)
    T=lambda u,v:(cx+u*ca-v*sa, cy+u*sa+v*ca)
    p0=T(0,0);p1=T(L,0); a=T(L*0.3,W);b=T(L*0.78,W*0.85);c=T(L*0.78,-W*0.85);d=T(L*0.3,-W)
    f=lambda p:f'{p[0]:.2f},{p[1]:.2f}'
    return f'M{f(p0)} C{f(a)} {f(b)} {f(p1)} C{f(c)} {f(d)} {f(p0)} Z', f'M{f(T(L*0.1,0))} L{f(T(L*0.82,0))}'
def offset_curve(fn, t0,t1,n=60): return [fn(t) for t in np.linspace(t0,t1,n)]

def blackfigure(ink, ground, lw=1.9, beard_locks=8, scale=1.0, knockout=False, pid='j', detail='full'):
    o=[]
    sild=d_of(PROF)+' '+' '.join(d_of(reverse(mirror(PROF))).split(' ')[1:])+' Z'
    # top leaves (outside silhouette) first, ink with incised veins
    tl=[];tv=[]
    for a,L in [(-1.57,26),(-1.2,22),(-1.94,22),(-0.9,17),(-2.24,17)]:
        q,v=leaf(0,14,a,L=L,W=6.4); tl.append(q); tv.append(v)
    o.append(f'<g fill="{ink}">'+''.join(f'<path d="{q}"/>' for q in tl)+'</g>')
    o.append(f'<path d="{NECK}" fill="{ink}"/>')
    o.append(f'<path d="{sild}" fill="{ink}"/>')
    inc=[]   # incised strokes (ground colour)
    for sx in (1,-1):
        # face boundary
        bd=d_of(FACE[8:] if False else [('M',(88,147))]+FACE[7:],mirror=(sx<0))
        inc.append((bd,lw))
        # eye: almond outline + pupil
        inc.append((f'M{P((63,116),sx)} C{P((67,112.5),sx)} {P((72,112.5),sx)} {P((77,115.5),sx)} M{P((64,117.5),sx)} C{P((68,121.5),sx)} {P((73,121),sx)} {P((76.5,117.5),sx)}',lw*0.9))
        # brow
        inc.append((f'M{P((60,107),sx)} C{P((66,103),sx)} {P((74,102.5),sx)} {P((81,105),sx)}',lw))
        # nostril & mouth
        inc.append((f'M{P((89.5,145.2),sx)} C{P((85.5,145.6),sx)} {P((84.6,141.2),sx)} {P((88,139.6),sx)}',lw*0.85))
        inc.append((f'M{P((84.5,159.5),sx)} L{P((79,159),sx)}',lw*0.9))
        if detail=='min': continue
        # ear (partly under hair)
        inc.append((f'M{P((42,112),sx)} C{P((34,108),sx)} {P((30,120),sx)} {P((33,128),sx)} C{P((35,134),sx)} {P((40,134),sx)} {P((43,130),sx)}',lw))
        inc.append((f'M{P((40,118),sx)} C{P((36,118),sx)} {P((36,124),sx)} {P((40,125),sx)}',lw*0.8))
        # hair: rows of small ringlets (archaic style), under the wreath
        WR=cubic((76,52),(60,24),(32,14),(8,16),n=60)
        step=8.6; rr=3.1
        for j,y in enumerate(np.arange(20,104,step*0.87)):
            for x in np.arange(2.5,90,step):
                xx=x+(step/2 if j%2 else 0); yy=y
                if not MPath(SIL).contains_point((xx,yy)): continue
                if MPath(FP).contains_point((xx,yy)): continue
                if np.min(np.hypot(FP[:,0]-xx,FP[:,1]-yy))<rr+3: continue
                if np.min(np.hypot(SIL[:,0]-xx,SIL[:,1]-yy))<rr+3.5: continue
                if np.min(np.hypot(WR[:,0]-xx,WR[:,1]-yy))<rr+7: continue
                if yy< np.interp(xx, WR[::-1,0], WR[::-1,1]): continue   # above wreath line
                if xx<rr+1.5: continue
                t=np.linspace(0,2*math.pi*1.05,20); r=rr*(0.25+0.75*t/t[-1])
                pts=[(xx+r_*math.cos(tt),yy-r_*math.sin(tt)) for tt,r_ in zip(t,r)]
                inc.append((pl(pts,sx),lw*0.72))
        # back hair locks down to the nape (behind the ear)
        for k in range(3):
            xs=26-k*9
            pts=[(xs+2.2*math.sin(v/6+k),v) for v in np.linspace(96,150,40)]
            inc.append((pl(pts,sx),lw*0.85))
        # forehead curls
        for (cx,cy) in [(72,60),(64,58),(56,62)]:
            t=np.linspace(0,2*math.pi*1.1,26); r=3.6*(0.3+0.7*t/t[-1])
            pts=[(cx+r_*math.cos(tt),cy+r_*math.sin(tt)) for tt,r_ in zip(t,r)]
            inc.append((pl(pts,sx),lw*0.8))
        # moustache
        inc.append((f'M{P((88,149),sx)} C{P((82,152),sx)} {P((76,154),sx)} {P((70,160),sx)}',lw*0.9))
        # beard locks: from the cheek/moustache line to the beard edge, ending in curls
        B=poly([('M',(86,151)),('C',(76,156),(62,153),(52,148)),('C',(44,143),(40,134),(39,124))])
        E=poly([('M',(80,206)),('C',(78,228),(66,244),(48,251)),('C',(34,256),(18,256),(9,252))])
        def at(Pl,t):
            d=np.r_[0,np.cumsum(np.hypot(*np.diff(Pl,axis=0).T))]; s_=t*d[-1]
            i=min(np.searchsorted(d,s_),len(Pl)-1); i=max(i,1)
            f=(s_-d[i-1])/(d[i]-d[i-1]+1e-9); return Pl[i-1]+(Pl[i]-Pl[i-1])*f
        n=beard_locks
        for i in range(n):
            t=(i+0.5)/n
            b=at(B,t); e=at(E,t)
            mid=(b+e)/2
            pts=[]
            for u in np.linspace(0,1,40):
                p=b+(e-b)*u
                p=p+np.array([4.2*math.sin(u*math.pi*2.0+0.6),0])*(math.sin(u*math.pi))
                pts.append(p)
            # curl turning towards the face's front
            cx,cy=pts[-1]+np.array([2.6,0])
            for a in np.linspace(math.pi,math.pi*2.75,18):
                r=2.6*(1-0.35*(a-math.pi)/(math.pi*1.75))
                pts.append(np.array([cx+r*math.cos(a), cy-r*math.sin(a)]))
            inc.append((pl(pts,sx),lw*0.85))
        # neck shading
        for k in range(4):
            x=27-k*4.2
            inc.append((f'M{P((x,266),sx)} L{P((x+0.6,288-k*1.2),sx)}',lw*0.6))
    # wreath (incised leaves on the hair band)
    leaves=[];veins=[]
    for sx in (1,-1):
        pts=cubic((76,52),(60,24),(32,14),(8,16),n=8)
        for i in range(len(pts)-1):
            x,y=pts[i]; dx,dy=pts[i+1]-pts[i]; ang=math.atan2(dy,dx)
            for s in (-1,1):
                a=ang+s*0.5
                if sx<0: a=math.pi-a
                q,v=leaf(sx*x,y,a,L=17,W=5.6)
                leaves.append(q); veins.append(v)
    if detail=='min':
        leaves=[];veins=[];tv=[]
    pupils=[f'<circle cx="{sx*71:.2f}" cy="117.2" r="1.9"/>' for sx in (1,-1)]
    tops=''.join(f'<path d="{q}"/>' for q in tl)
    cuts=''.join(f'<path d="{d}" stroke-width="{w:.2f}"/>' for d,w in inc)
    leafo=''.join(f'<path d="{q}"/>' for q in leaves)
    vein=''.join(f'<path d="{v}"/>' for v in veins+tv)
    if not knockout:
        # flat: incisions painted in the ground colour (needs that background)
        return (f'<g id="{pid}-figura" fill="{ink}"><g id="{pid}-ramo">{tops}</g><path id="{pid}-pescoco" d="{NECK}"/><path id="{pid}-silhueta" d="{sild}"/></g>'
                f'<g id="{pid}-incisoes" fill="none" stroke="{ground}" stroke-linecap="round" stroke-linejoin="round">{cuts}</g>'
                f'<g id="{pid}-louros" fill="{ink}" stroke="{ground}" stroke-width="{lw*0.75:.2f}">{leafo}</g>'
                f'<g id="{pid}-nervuras" fill="none" stroke="{ground}" stroke-width="{lw*0.5:.2f}">{vein}</g>'
                f'<g id="{pid}-pupilas" fill="{ground}">{"".join(pupils)}</g>')
    # knockout: incisions cut through the figure (transparent), via a mask
    return (f'<mask id="{pid}-mask" maskUnits="userSpaceOnUse" x="-200" y="-200" width="400" height="700">'
            f'<rect x="-200" y="-200" width="400" height="700" fill="#fff"/>'
            f'<g id="{pid}-incisoes" fill="none" stroke="#000" stroke-linecap="round" stroke-linejoin="round">{cuts}</g>'
            f'<g id="{pid}-louros" fill="none" stroke="#000" stroke-width="{lw*0.75:.2f}">{leafo}</g>'
            f'<g id="{pid}-nervuras" fill="none" stroke="#000" stroke-width="{lw*0.5:.2f}">{vein}</g>'
            f'<g id="{pid}-pupilas" fill="#000">{"".join(pupils)}</g></mask>'
            f'<g id="{pid}-figura" fill="{ink}" mask="url(#{pid}-mask)"><g id="{pid}-ramo">{tops}</g><path id="{pid}-pescoco" d="{NECK}"/><path id="{pid}-silhueta" d="{sild}"/></g>')
if __name__=='__main__':
    html='<html><body style="margin:0;display:flex">'
    for ink,g in [('#17140F','#ECE5D6'),('#17140F','#C4693A')]:
        html+=f'<div style="background:{g};padding:20px"><svg width="560" height="640" viewBox="-115 -30 230 330">{blackfigure(ink,g)}</svg></div>'
    open('bf.html','w').write(html+'</body></html>')
