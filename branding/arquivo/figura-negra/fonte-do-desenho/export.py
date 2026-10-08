import os, json, pathlib, shutil
from classic_parts import *
from playwright.sync_api import sync_playwright
OUT=pathlib.Path('saida'); 
if OUT.exists(): shutil.rmtree(OUT)
for d in ['svg','pdf','png','favicon']: (OUT/d).mkdir(parents=True,exist_ok=True)
def svgdoc(inner,vb,title):
    x,y,w,h=map(float,vb.split())
    return (f'<?xml version="1.0" encoding="UTF-8"?>\n<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}" width="{w:.0f}" height="{h:.0f}">'
            f'<title>{title}</title>{inner}</svg>\n')
F={}
g,vb=lockup_classic_v(BLK,BONE,pid='v'); F['janus-vertical']=(g,vb,'Janus — assinatura vertical')
g,vb=lockup_classic_v(BONE,BLK,pid='vn'); F['janus-vertical-negativo']=(g,vb,'Janus — vertical, para fundos escuros')
g,vb=lockup_classic_v(TER,BLK,pid='vr'); F['janus-vertical-figura-vermelha']=(g,vb,'Janus — vertical, figura vermelha (fundo preto)')
g,vb=lockup_classic_h(BLK,BONE,pid='h'); F['janus-horizontal']=(g,vb,'Janus — assinatura horizontal')
g,vb=lockup_classic_h(BONE,BLK,pid='hn'); F['janus-horizontal-negativo']=(g,vb,'Janus — horizontal, para fundos escuros')
g,vb=lockup_classic_h(TER,BLK,pid='hr'); F['janus-horizontal-figura-vermelha']=(g,vb,'Janus — horizontal, figura vermelha')
g,vb=mark(BLK,BONE,pid='m'); F['janus-simbolo']=(g,vb,'Janus — símbolo')
g,vb=mark(BONE,BLK,pid='mn'); F['janus-simbolo-negativo']=(g,vb,'Janus — símbolo, para fundos escuros')
g,vb=mark(TER,BLK,pid='mr'); F['janus-simbolo-figura-vermelha']=(g,vb,'Janus — símbolo, figura vermelha')
g,vb=coin(BLK,TER,TER,pid='c'); F['janus-moeda']=(g,vb,'Janus — moeda')
g,vb=coin(TER,BLK,BLK,pid='cr'); F['janus-moeda-escura']=(g,vb,'Janus — moeda escura')
wd,w=classic_word(BLK,size=100,pid='n'); F['janus-nome']=(wd,f'-4 -84 {w+8} 112','Janus — logotipo')
wd,w=classic_word(BONE,size=100,pid='nn'); F['janus-nome-negativo']=(wd,f'-4 -84 {w+8} 112','Janus — logotipo, negativo')
PF={}
for name,fn,args,bg in [
 ('janus-vertical',lockup_classic_v,(BLK,BONE),BONE),('janus-vertical-negativo',lockup_classic_v,(BONE,BLK),BLK),
 ('janus-vertical-figura-vermelha',lockup_classic_v,(TER,BLK),BLK),('janus-horizontal',lockup_classic_h,(BLK,BONE),BONE),
 ('janus-horizontal-negativo',lockup_classic_h,(BONE,BLK),BLK),('janus-horizontal-figura-vermelha',lockup_classic_h,(TER,BLK),BLK),
 ('janus-simbolo',mark,(BLK,BONE),BONE),('janus-simbolo-negativo',mark,(BONE,BLK),BLK),('janus-simbolo-figura-vermelha',mark,(TER,BLK),BLK)]:
    g,vb=fn(*args,pid='p',knockout=False)
    x,y,w,h=map(float,vb.split())
    PF[name]=(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{bg}"/>'+g,vb)
g,vb=coin(BLK,TER,TER,pid='p',knockout=False); PF['janus-moeda']=(g,vb)
g,vb=coin(TER,BLK,BLK,pid='p',knockout=False); PF['janus-moeda-escura']=(g,vb)
PF['janus-nome']=F['janus-nome'][:2]; PF['janus-nome-negativo']=F['janus-nome-negativo'][:2]
# favicon: simplified coin
g,vb=coin(BLK,TER,TER,pid='f',detail='min',beads=False,k=0.98); FAV=(g,vb)
for name,(g,vb,t) in F.items(): (OUT/'svg'/f'{name}.svg').write_text(svgdoc(g,vb,t))
(OUT/'favicon'/'favicon.svg').write_text(svgdoc(FAV[0],FAV[1],'Janus'))
# editable text version of the wordmark (needs TeX Gyre Pagella installed)
(OUT/'svg'/'janus-nome-texto-editavel.svg').write_text(svgdoc(
  f'<text x="0" y="0" font-family="\'TeX Gyre Pagella\', \'Palatino Linotype\', Palatino, serif" font-size="100" letter-spacing="16" fill="{BLK}">JANUS</text>','-4 -84 400 112','Janus — logotipo (texto editável)'))
# render PDF + PNG
with sync_playwright() as p:
    b=p.chromium.launch(); pg=b.new_page()
    def page_for(svgtext, W, H):
        html=f'<html><head><style>@page{{size:{W}px {H}px;margin:0}}html,body{{margin:0;background:transparent}}svg{{display:block;width:{W}px;height:{H}px}}</style></head><body>{svgtext}</body></html>'
        pg.set_viewport_size({'width':int(W),'height':int(H)}); pg.set_content(html)
    for name,(g,vb,t) in list(F.items()):
        x,y,w,h=map(float,vb.split())
        fs=svgdoc(*PF[name],t).split('\n',1)[1]
        page_for(fs,w,h); pg.pdf(path=str(OUT/'pdf'/f'{name}.pdf'),width=f'{w}px',height=f'{h}px',print_background=True)
        s=svgdoc(g,vb,t).split('\n',1)[1]
        for W in ([2048,1024,512] if not name.startswith('janus-nome') else [2048,1024]):
            H=round(W*h/w)
            page_for(s,W,H); pg.screenshot(path=str(OUT/'png'/f'{name}-{W}.png'),omit_background=True)
    x,y,w,h=map(float,FAV[1].split()); s=svgdoc(*FAV,'Janus').split('\n',1)[1]
    for W in [16,32,48,64,180,192,512]:
        page_for(s,W,W); pg.screenshot(path=str(OUT/'favicon'/f'favicon-{W}.png'),omit_background=True)
    # full-detail coin for large icons
    g,vb=F['janus-moeda'][:2]; s=svgdoc(g,vb,'Janus').split('\n',1)[1]
    for W in [256,512,1024]:
        page_for(s,W,W); pg.screenshot(path=str(OUT/'favicon'/f'icone-moeda-{W}.png'),omit_background=True)
    b.close()
from PIL import Image
imgs=[Image.open(OUT/'favicon'/f'favicon-{s}.png') for s in (16,32,48)]
big=Image.open(OUT/'favicon'/'icone-moeda-256.png')
big.save(OUT/'favicon'/'janus.ico', sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)], append_images=imgs)
print(sorted(str(p.relative_to(OUT)) for p in OUT.rglob('*') if p.is_file()))
