# Janus — identidade visual

O olho bitemporal de Janus (*"Look back. Look ahead."*): duas ópticas/horizontes integrados que olham simultaneamente para o passado (à esquerda) e para o futuro (à direita). A íris é composta por estratos espectrais concêntricos (representando as camadas multitemporais da pilha raster) e um sensor focal central com o marcador de data presente em laranja (`#F29A38`), exatamente a cor da linha de tempo do ImPlot no Janus. O nome usa letras condensadas desenhadas para combinar com a geometria. As cores vêm da própria interface do programa (ImGui/ImPlot).

## Cores

| Nome | Hex | Uso |
|---|---|---|
| Azul ImGui | `#4296FA` | Cor principal da óptica e asa do passado |
| Azul Claro | `#6AADFF` / `#CEE3FF` | Realces e versão clara do logo |
| Ciano Espectral | `#00F2FE` | Asa do futuro e detecção de anomalia |
| Marcador de Data ("Now") | `#F29A38` | Ponto focal do sensor central e eixos temporais |
| Janela | `#0F0F0F` | Fundo principal da interface |
| Painel | `#162447` / `#1C2A3C` | Fundo do ícone e áreas de destaque |

## Arquivos

| Pasta / Arquivo | Formato | Para quê |
|---|---|---|
| `janus-olho-editavel.svg` | SVG vetorial com camadas | **Arquivo mestre editável** (Inkscape, Illustrator, Figma, Affinity) com IDs semânticos |
| `svg/` | SVG vetorial | Versões de produção para web e sistemas (`janus-simbolo`, `janus-horizontal`, `janus-vertical`, `janus-icone`) |
| `png/` | PNG transparente, 512 / 1024 / 2048 px de largura | Documentos, slides, README |
| `icones/` | `janus.ico`, `janus.icns`, PNG 64–1024 | Ícone do programa no Windows, macOS e avatar (`icone-quadrado-*`) |
| `favicon/` | SVG, PNG 16–48 px, `favicon.ico` | Site e abas do navegador |
| `tools/generate_brand_eye.py` | Python (Pillow; PNGs drawn by headless Chrome or Edge, PyMuPDF as a fallback without gradients) | Regenera todo o sistema de assets de forma determinística |

Variações em `svg/` e `png/`: `janus-simbolo`, `janus-horizontal`, `janus-vertical` e `janus-nome`, cada uma em azul (sem sufixo), `-claro` e `-escuro`. `janus-icone` é o ícone com cantos arredondados para docks e atalhos; `janus-icone-quadrado` é sem cantos, para lugares que arredondam sozinhos (iOS, avatar do GitHub).

## Como o SVG Master está montado

O arquivo `branding/janus-olho-editavel.svg` contém grupos semânticos:
- `#contornos`: Asas externas do olho (`#asa-esquerda` com gradiente de passado e `#asa-direita` com gradiente de futuro);
- `#iris`: Camadas concêntricas de estratos espectrais (`#iris-passado` e `#iris-futuro`);
- `#meridianos`: Marcadores verticais do meridiano de tempo e setas direcionais no horizonte;
- `#cursor-data`: Ponto focal e anel de brilho do sensor central (`#F29A38`).

## Gerar de novo

```bash
python tools/generate_brand_eye.py
```
O script gera os SVGs, renderiza os PNGs em todas as escalas através do PyMuPDF, compila o `janus.ico` multi-resolução e o `janus.icns` do macOS, e sincroniza automaticamente com as pastas `resources/` e `site/`.

A proposta anterior (figura negra) está guardada em `arquivo/figura-negra/`.
