from matplotlib.textpath import TextPath
from matplotlib.font_manager import FontProperties
import numpy as np
import os
_F=os.path.join(os.path.dirname(os.path.abspath(__file__)),'..','fontes')
FONTS={'pagella':os.path.join(_F,'texgyrepagella-regular.otf'),
       'pagella-b':os.path.join(_F,'texgyrepagella-bold.otf')}
def text_d(text, font='pagella', size=100, track=0.0, x0=0, y0=0):
    """SVG path data for text; baseline at y0, cap height ~0.7*size. returns (d, width)"""
    fp=FontProperties(fname=FONTS[font])
    x=0; ds=[]
    for ch in text:
        tp=TextPath((0,0), ch, size=size, prop=fp)
        verts=tp.vertices; codes=tp.codes
        adv=TextPath((0,0), ch+'|', size=size, prop=fp).get_extents().x1 - TextPath((0,0), '|', size=size, prop=fp).get_extents().x1
        d=[];i=0
        while i<len(codes):
            c=codes[i]; v=verts[i]
            X=lambda p:f"{x0+x+p[0]:.2f},{y0-p[1]:.2f}"
            if c==1: d.append('M'+X(v)); i+=1
            elif c==2: d.append('L'+X(v)); i+=1
            elif c==3: d.append('Q'+X(verts[i])+' '+X(verts[i+1])); i+=2
            elif c==4: d.append('C'+X(verts[i])+' '+X(verts[i+1])+' '+X(verts[i+2])); i+=3
            elif c==79: d.append('Z'); i+=1
            else: i+=1
        ds.append(' '.join(d))
        ext=tp.get_extents()
        x+= (ext.x1 if ch!=' ' else size*0.3) + track
    return ' '.join(ds), x-track
if __name__=='__main__':
    for f in ['pagella','termes']:
        d,w=text_d('JANUS',f,100,track=14)
        print(f,w)
