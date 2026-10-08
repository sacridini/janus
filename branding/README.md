# Janus — identidade visual

A cabeça dupla de Janus: as duas faces são sólidas e o cabelo e a barba são uma
grade de células, como os pixels de um raster. Uma face olha para o passado, a
outra para o futuro. O nome usa letras condensadas desenhadas para combinar com o
traço. As cores vêm da própria interface do programa (ImGui).

## Cores

| Nome | Hex | Uso |
|---|---|---|
| Azul ImGui | `#4296FA` | cor principal do logo; funciona em fundo escuro e claro |
| Claro | `#CEE3FF` | logo sobre azul ou azul-marinho |
| Escuro | `#14243A` | logo em uma cor para fundos claros e impressão |
| Janela | `#0F0F0F` | fundo |
| Painel | `#1C2A3C` | fundo do ícone, áreas de destaque |
| Marcador de data | `#F29A38` | só como acento (gráficos, links), nunca no logo |

## Arquivos

| Pasta | Formato | Para quê |
|---|---|---|
| `svg/` | SVG vetorial | **arquivo mestre para editar** (Inkscape, Illustrator, Figma, Affinity) e para a web |
| `pdf/` | PDF vetorial, fundo transparente | impressão e Illustrator |
| `png/` | PNG transparente, 512 / 1024 / 2048 px de largura | documentos, slides, README |
| `icones/` | `janus.ico`, `janus.icns`, PNG 64–1024 | ícone do programa e avatar do GitHub (`icone-quadrado-*`) |
| `favicon/` | pixel art exato em 16, 24, 32 e 48 px (SVG e PNG), `favicon.ico` | site e abas do navegador |
| `fonte-do-desenho/` | Python | gera tudo de novo a partir das coordenadas |

Variações em `svg/`, `pdf/` e `png/`: `janus-simbolo`, `janus-horizontal`,
`janus-vertical` e `janus-nome`, cada uma em azul (sem sufixo), `-claro` e
`-escuro`. `janus-icone` é o ícone com cantos arredondados; `janus-icone-quadrado`
é sem cantos, para lugares que arredondam sozinhos (iOS, avatar do GitHub).

## Como os SVGs estão montados

Grupos nomeados: `simbolo` (as células do cabelo e da barba são `rect`, as faces
são dois `path` com o olho vazado) e `nome` (as letras são traços, `stroke`,
recortados por um `clipPath` na altura das maiúsculas). Para mudar a cor, troque
o `fill` do símbolo e o `stroke` do nome.

O favicon não é uma redução do desenho grande: é pixel art desenhado na grade de
cada tamanho, para ficar nítido em 16 e 32 px.

## Regras de uso

- Espaço livre em volta: pelo menos a largura de três células.
- Abaixo de 64 px use os arquivos de `favicon/`.
- O laranja não entra no logo; ele é o acento do site e dos gráficos.
- Não aplique contorno, sombra, gradiente nem distorção.

## Gerar de novo

```bash
cd fonte-do-desenho
python export_px.py   # gera tudo em fonte-do-desenho/saida/ (numpy, matplotlib, pillow, playwright)
```

O perfil está em `c2.py` (`PROF`, `face`, `EYE`), a grade em `scanvar.py`
(`v_pixel`), o nome em `word.py` e o pixel art em `pixart.py`.

A proposta anterior (figura negra) está guardada em `arquivo/figura-negra/`.
