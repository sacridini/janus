# Janus — identidade visual

Cabeça dupla de Janus no estilo dos vasos gregos de **figura negra**: a figura é
uma massa de cor e os detalhes (olhos, louros, cabelo, barba) são riscos que
deixam aparecer o fundo. O nome vai em capitais romanas (TeX Gyre Pagella).

## Cores

| Nome | Hex | Uso |
|---|---|---|
| Preto ático | `#17140F` | figura, texto |
| Terracota | `#C2683A` | barro do vaso; fundo da moeda, figura vermelha |
| Papel | `#ECE5D6` | fundo claro; figura em fundos escuros |

## Arquivos

| Pasta | Formato | Para quê |
|---|---|---|
| `svg/` | SVG vetorial | **arquivo mestre para editar** (Inkscape, Illustrator, Figma, Affinity) e para a web |
| `pdf/` | PDF vetorial | impressão e Illustrator; cada PDF já traz a cor de fundo pensada para ele |
| `png/` | PNG com fundo transparente, 512 / 1024 / 2048 px de largura | documentos, slides, README |
| `favicon/` | SVG, PNG 16–512, `janus.ico` | site, ícone do app, avatar do GitHub (`icone-moeda-*.png`) |
| `fontes/` | TeX Gyre Pagella (OTF) | para editar o nome como texto |
| `fonte-do-desenho/` | Python | gera tudo de novo a partir das coordenadas |

Variações em `svg/`, `pdf/` e `png/`:

- `janus-vertical`, `janus-horizontal`, `janus-simbolo`: preto, para fundos claros
- `…-negativo`: cor de papel, para fundos escuros
- `…-figura-vermelha`: terracota, para fundo preto (como os vasos de figura vermelha)
- `janus-moeda`, `janus-moeda-escura`: medalhão com borda de pérolas
- `janus-nome`, `janus-nome-negativo`: só o nome; `janus-nome-texto-editavel.svg` tem o nome como texto (instale a fonte de `fontes/`)

## Como os SVGs estão montados

Cada SVG tem grupos nomeados: `cabeca`, `silhueta`, `pescoco`, `ramo` (folhas do
topo), `incisoes` (todos os riscos), `louros`, `nervuras`, `pupilas`, `meandro`
e `nome`. Os riscos são **traços** (stroke), não contornos, então dá para mudar a
espessura de uma vez. Eles ficam dentro de uma **máscara** (`mask`) que recorta a
figura: por isso o fundo aparece através dos riscos, em qualquer cor de fundo.

Para mudar a cor da figura, altere o `fill` do grupo `…-figura`.

## Regras de uso

- Espaço livre em volta do logo: pelo menos a altura das letras do nome.
- Tamanho mínimo: assinatura vertical com 80 px de largura; abaixo disso use a moeda ou o favicon.
- Abaixo de 64 px use `favicon/` (versão simplificada, só silhueta, perfil e olhos).
- Não aplique contorno, sombra, gradiente nem distorção.

## Gerar de novo

```bash
cd fonte-do-desenho
python export.py      # gera tudo em fonte-do-desenho/saida/ (requer numpy, matplotlib, pillow e playwright)
```

O perfil está em `bf.py` (`PROF`, `FACE`); o friso, as moedas e as assinaturas em
`classic_parts.py`.
