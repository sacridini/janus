# Janus — identidade visual

**◁ | ▷** — retroceder e avançar, como nos botões de um videocassete. Dois triângulos vazados num mesmo eixo: o da esquerda (o passado) mais alto, o da direita (o futuro) mais baixo, e o eixo passando dos dois. O símbolo segue as regras do nome: o mesmo traço, pontas retas, cantos em quina, uma cor só, alturas desencontradas e hastes que vazam a caixa, como no A e no N.

## Construção

Unidades do wordmark, em que a altura das letras vai de 0 a 100.

| Elemento | Desenho |
|---|---|
| Traço | 8,5, pontas retas (*butt*), cantos em quina (*miter*), igual às letras |
| Eixo | x = 50, de y = −30 a y = 130 (as mesmas pontas do N) |
| Passado ◁ | (50, −20) → (2, 30) → (50, 80) |
| Futuro ▷ | (50, 20) → (98, 70) → (50, 120), o passado girado 180° |
| Folga até o J | 44, a mesma ordem da folga entre as letras |

Nos ícones de 16, 24 e 32 px o símbolo é desenhado pixel a pixel, com linhas de 1 px (o de 32 é o de 16 dobrado). Em 48 px o traço fica mais grosso para não sumir.

## Cores

| Nome | Hex | Uso |
|---|---|---|
| Azul ImGui | `#4296FA` | Cor do símbolo e do nome |
| Azul claro | `#CEE3FF` | Versões `-claro`, sobre fundos escuros ou coloridos |
| Azul escuro | `#1D4ED8` | Versões `-escuro`, sobre branco (README no tema claro) |
| Janela | `#0F0F0F` | Fundo do programa e do site |
| Ladrilho | `#111A2E` / `#2A4A7A` | Fundo e borda do ícone do programa |

## Arquivos

| Pasta / arquivo | Formato | Para quê |
|---|---|---|
| `janus-editavel.svg` | SVG com camadas | **Arquivo mestre editável** (Inkscape, Illustrator, Figma, Affinity): camadas `simbolo`, `nome` e `guias` (escondida, com as linhas de construção) |
| `janus-editavel.pdf` | PDF vetorial | O mesmo mestre para quem prefere abrir PDF no Illustrator ou no Affinity |
| `svg/` | SVG | `janus-simbolo`, `janus-horizontal`, `janus-vertical`, `janus-nome`, cada um em azul (sem sufixo), `-claro` e `-escuro`; `janus-icone` (cantos arredondados) e `janus-icone-quadrado` |
| `pdf/` | PDF vetorial | Os mesmos desenhos de `svg/` |
| `png/` | PNG transparente, 512 / 1024 / 2048 px de largura | Documentos, slides, README; `janus-social-1280x640.png` é a imagem de prévia do repositório no GitHub |
| `icones/` | `janus.ico`, `janus.icns`, PNG 64–1024 | Ícone do programa e avatar (`icone-quadrado-*`) |
| `favicon/` | SVG, PNG 16–48 px, `favicon.ico` | Site e abas do navegador |

## Gerar de novo

```bash
python tools/generate_brand.py
```

O script desenha todos os SVGs e PDFs, os PNGs (pelo Chrome ou Edge em modo headless; aponte a variável `CHROME` para outro navegador), os ícones `.ico` e `.icns`, a imagem `og.png` do site, e copia o que precisa para `resources/` (ícone do programa) e `site/`.

## Propostas anteriores

- `arquivo/olho/` — o olho bitemporal (até outubro de 2026), com o script que o gerava.
- `arquivo/figura-negra/` — a figura negra.
- `opcoes-anos-90/`, `10-logos-janus/`, `novas-opcoes-lote2/`, `propostas-novas/` — rodadas de opções.
