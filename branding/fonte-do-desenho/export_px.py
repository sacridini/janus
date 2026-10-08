"""Janus brand package (Pixel mark). Run: python export_px.py  -> ./saida/"""
import pathlib, shutil, itertools
from scanvar import v_pixel
from word import wordmark
import pixart
from playwright.sync_api import sync_playwright
from PIL import Image
BLUE='#4296FA'; PALE='#CEE3FF'; DARK='#14243A'; WIN='#0F0F0F'; NAVY='#1C2A3C'
OUT=pathlib.Path('saida')
if OUT.exists(): shutil.rmtree(OUT)
for d in ['svg','pdf','png','icones','favicon']: (OUT/d).mkdir(parents=True)
uid=itertools.count()
WM=dict(s=8.5,W=48,track=24)
def doc(inner,vb,title):
    x,y,w,h=map(float,vb.split())
    return f'<?xml version="1.0" encoding="UTF-8"?>\n<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}" width="{w:.0f}" height="{h:.0f}"><title>{title}</title>{inner}</svg>\n'
def mark(c): return f'<g id="simbolo">{v_pixel(c)}</g>', '-100 -8 200 270'
def word(c):
    g,t=wordmark(color=c,cid=f'n{next(uid)}',**WM); return f'<g id="nome">{g}</g>', f'-6 -6 {t+12} 112'
def horiz(c):
    g,t=wordmark(color=c,x0=135,y0=127-52,scale=1.04,cid=f'h{next(uid)}',**WM)
    return f'<g id="simbolo">{v_pixel(c)}</g><g id="nome">{g}</g>', f'-100 -8 {135+t*1.04+112} 270'
def vert(c):
    _,t=wordmark(**WM); s=0.8; x0=-t*s/2
    g,_=wordmark(color=c,x0=x0,y0=300,scale=s,cid=f'v{next(uid)}',**WM)
    return f'<g id="simbolo">{v_pixel(c)}</g><g id="nome">{g}</g>', f'{x0-24} -8 {t*s+48} 396'
def icon(fg,bg,rx=66,k=0.9):
    return (f'<rect id="fundo" x="-150" y="-23" width="300" height="300" rx="{rx}" fill="{bg}"/>'
            f'<g id="simbolo" transform="translate(0,{127-127*k:.2f}) scale({k})">{v_pixel(fg)}</g>'), '-150 -23 300 300'
F={}
for name,fn in [('janus-simbolo',mark),('janus-horizontal',horiz),('janus-vertical',vert),('janus-nome',word)]:
    for suf,c in [('',BLUE),('-claro',PALE),('-escuro',DARK)]:
        g,vb=fn(c); F[name+suf]=(g,vb,'Janus')
F['janus-icone']=(*icon(BLUE,NAVY),'Janus')
F['janus-icone-quadrado']=(*icon(BLUE,NAVY,rx=0),'Janus')
for n,(g,vb,t) in F.items(): (OUT/'svg'/f'{n}.svg').write_text(doc(g,vb,t))
# pixel-art favicons (exact pixels)
PA={}
for n in (16,24,32,48):
    A=pixart.pattern(pixart.grid(n,eyes=n>=24,gapdist=0.3),'dots',0); PA[n]=A
    svg=pixart.to_svg(A,BLUE,BLUE)
    (OUT/'favicon'/f'favicon-{n}.svg').write_text(svg)
    im=Image.new('RGBA',(n,n),(0,0,0,0)); px=im.load()
    rgb=tuple(int(BLUE[i:i+2],16) for i in (1,3,5))+(255,)
    for r in range(n):
        for q in range(n):
            if A[r,q]: px[q,r]=rgb
    im.save(OUT/'favicon'/f'favicon-{n}.png')
shutil.copy(OUT/'favicon'/'favicon-32.svg', OUT/'favicon'/'favicon.svg')
with sync_playwright() as p:
    b=p.chromium.launch(); pg=b.new_page()
    def load(svgtext,W,H):
        pg.set_viewport_size({'width':max(1,int(W)),'height':max(1,int(H))})
        pg.set_content(f'<html><head><style>@page{{size:{W}px {H}px;margin:0}}html,body{{margin:0;background:transparent}}svg{{display:block;width:{W}px;height:{H}px}}</style></head><body>{svgtext}</body></html>')
    for n,(g,vb,t) in F.items():
        x,y,w,h=map(float,vb.split()); s=doc(g,vb,t).split('\n',1)[1]
        load(s,w,h); pg.pdf(path=str(OUT/'pdf'/f'{n}.pdf'),width=f'{w}px',height=f'{h}px',print_background=True)
        for W in [512,1024,2048]:
            load(s,W,round(W*h/w)); pg.screenshot(path=str(OUT/'png'/f'{n}-{W}.png'),omit_background=True)
    for W in [64,128,180,192,256,512,1024]:
        g,vb,t=F['janus-icone']; load(doc(g,vb,t).split('\n',1)[1],W,W)
        pg.screenshot(path=str(OUT/'icones'/f'icone-{W}.png'),omit_background=True)
        g,vb,t=F['janus-icone-quadrado']; load(doc(g,vb,t).split('\n',1)[1],W,W)
        pg.screenshot(path=str(OUT/'icones'/f'icone-quadrado-{W}.png'),omit_background=True)
    b.close()
# Windows .ico: pixel art for 16-48, drawn icon above
ico=[Image.open(OUT/'icones'/'icone-256.png')]
imgs=[Image.open(OUT/'favicon'/f'favicon-{n}.png') for n in (16,24,32,48)]+[Image.open(OUT/'icones'/f'icone-{n}.png') for n in (64,128)]
ico[0].save(OUT/'icones'/'janus.ico',sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)],append_images=imgs)
shutil.copy(OUT/'icones'/'janus.ico', OUT/'favicon'/'favicon.ico')
try:
    Image.open(OUT/'icones'/'icone-1024.png').save(OUT/'icones'/'janus.icns')
except Exception as e: print('icns:',e)
print(sum(1 for _ in OUT.rglob('*') if _.is_file()),'files')
