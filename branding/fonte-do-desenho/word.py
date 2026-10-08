# custom geometric condensed wordmark JANUS; cap height 100
def letters(s=9.0, W=50.0):
    h=s/2; R=(W-s)/2; H=100
    L={}
    L['J']=(W, f'M{W-h},-20 L{W-h},{H-R-h} A{R},{R} 0 0 1 {h},{H-R-h} L{h},{H-R-h-8}')
    A_w=W+6
    L['A']=(A_w, f'M{h-6},{H+40} L{A_w/2},{-s*0.9} L{A_w-h+6},{H+40} M{A_w*0.2},{H*0.66} L{A_w*0.8},{H*0.66}')
    L['N']=(W, f'M{h},{H+30} L{h},{-s*0.2} L{W-h},{H+s*0.2} L{W-h},-30')
    L['U']=(W, f'M{h},-20 L{h},{H-R-h} A{R},{R} 0 0 0 {W-h},{H-R-h} L{W-h},-20')
    r=(W-s)/2; ct=r+h; cb=H-r-h
    L['S']=(W, f'M{W-h},{ct+5} L{W-h},{ct} A{r},{r} 0 0 0 {h},{ct} C{h},{ct+24} {W-h},{cb-24} {W-h},{cb} A{r},{r} 0 0 1 {h},{cb} L{h},{cb-5}')
    return L
def wordmark(text='JANUS', color='#000', s=9.0, W=50.0, track=26.0, x0=0, y0=0, scale=1.0, cid='wm'):
    L=letters(s,W); x=0; parts=[]
    for ch in text:
        w,d=L[ch]
        parts.append(f'<path transform="translate({x},0)" d="{d}"/>'); x+=w+track
    total=x-track
    g=(f'<clipPath id="{cid}"><rect x="-10" y="0" width="{total+20}" height="100"/></clipPath>'
       f'<g transform="translate({x0},{y0}) scale({scale})"><g clip-path="url(#{cid})" fill="none" stroke="{color}" stroke-width="{s}" stroke-linejoin="miter" stroke-miterlimit="12">{"".join(parts)}</g></g>')
    return g,total
if __name__=='__main__':
    html='<html><body style="margin:0;background:#EEE9DF;padding:30px">'
    for s,W,t in [(9,50,26),(6,46,30),(12,54,22)]:
        g,tot=wordmark(s=s,W=W,track=t,cid=f'c{s}')
        html+=f'<svg width="{(tot+20)*1.2}" height="140" viewBox="-10 -10 {tot+20} 120" style="display:block;margin-bottom:20px">{g}</svg>'
    open('word.html','w').write(html+'</body></html>')
