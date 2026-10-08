from janus import *
PROF=[('M',(0,0)),
 ('C',(40,0),(68,14),(72,44)),
 ('C',(74,52),(71,58),(66,61)),
 ('L',(69,93)),
 ('C',(71,96),(72,99),(70,102)),
 ('C',(69,104),(69,105),(70,107)),
 ('L',(90,140)),
 ('C',(92,144),(90,148),(85,148)),
 ('L',(77,149)),
 ('L',(80,157)),
 ('L',(74,160)),
 ('L',(78,165)),
 ('C',(80,168),(83,171),(84,178)),
 ('C',(85,200),(78,222),(60,238)),
 ('C',(44,250),(24,254),(0,254))]
# face region (right): along profile from hairline to lower lip, then back along beard/hair boundary
def sub(segs,a,b):  # segments index a..b inclusive (after M)
    return segs[a:b+1]
face=[('M',(66,61))]+PROF[3:12]+[
 ('C',(70,168),(60,170),(50,166)),
 ('C',(40,161),(34,150),(34,136)),
 ('L',(34,104)),
 ('C',(34,82),(44,64),(66,61))]
EYE=[('M',(55,112)),('C',(59,108.5),(63,108),(67,108.5)),('C',(65.5,110.5),(65.5,112.5),(66.5,114.5)),('C',(62,115),(58,114),(55,112))]
right=PROF; left=reverse(mirror(PROF))
sil=np.vstack([poly(PROF), poly(left)[1:]])
faceR=poly(face); faceL=faceR*[-1,1]
eyeR=poly(EYE)
def stripes(period=6.0, th=3.4, y0=0, y1=256, jag=0.0):
    rects=[]
    y=y0+period/2
    k=0
    while y<y1:
        sp=hspans(sil,y)
        cuts=[]
        for F in (faceR,faceL):
            for a,b in hspans(F,y): cuts.append((a-2.2,b+2.2))
        for a,b in subtract(sp,cuts):
            if jag:
                j=jag*(0.5+0.5*math.sin(k*1.9))
                a2=a+ (j if a<-30 else 0); b2=b-(j if b>30 else 0)
            else: a2,b2=a,b
            if b2-a2>1: rects.append((a2,y-th/2,b2-a2,th))
        y+=period;k+=1
    return rects
def svg_mark(color, bg=None, period=6.0, th=3.4, jag=0, eye=True, split=False):
    r=stripes(period,th,jag=jag)
    body=''.join(f'<rect x="{x:.2f}" y="{y:.2f}" width="{w:.2f}" height="{h:.2f}" rx="{h/2:.2f}"/>' for x,y,w,h in r)
    fd=d_of(face,close=True); fl=d_of(face,mirror=True,close=True)
    e=d_of(EYE,close=True); el=d_of(EYE,mirror=True,close=True)
    bgc=bg or 'transparent'
    return f'''<g fill="{color}">{body}<path d="{fd} {e}" fill-rule="evenodd"/><path d="{fl} {el}" fill-rule="evenodd"/></g>'''
if __name__=='__main__':
    html='<html><body style="margin:0;background:#EEE9DF;display:flex;gap:30px;padding:30px;flex-wrap:wrap">'
    for args in [dict(),dict(period=7,th=4.2),dict(period=5,th=2.6)]:
        html+=f'<svg width="380" height="380" viewBox="-130 -40 260 360">{svg_mark("#12302B",**args)}</svg>'
    html+='</body></html>'
    open('c2.html','w').write(html)
