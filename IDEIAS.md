# IDEIAS — tsv

Registro de ideias, decisões (com os números que as motivaram) e avanços do
projeto. Atualizado a cada fase.

## Visão

Um visualizador **especializado e muito rápido** para séries temporais raster e
cubos de dados: abrir, navegar no tempo, inspecionar a série de qualquer pixel e
rodar análises de mudança/tendência — sem ser um SIG completo. Instalável no
Windows sem Python nem conda, e chamável pela linha de comando
(`tsv serie.tif`).

## Decisões (e por quê)

### Dear ImGui + OpenGL, não uma GUI própria em Vulkan
- O trabalho de GPU é mínimo: estatísticas temporais de 5,5 Mpx × 41 datas em
  ~90 ms (uma vez); um quadro custa 0,2–0,4 ms de CPU.
- O gargalo é disco/descompressão, que Vulkan não resolve. Reescrever texto,
  widgets, docking e gráficos custaria meses sem ganho perceptível.
- O ImGui tem backend Vulkan, e o código de GPU está isolado em `gpu.cpp`: se um
  dia houver gargalo medido de GPU, a troca é localizada.

### Leitura em HD mecânico
- Medido a frio (GeoTIFF LZW em faixas de 1 linha, ~200 MB/data):
  - 1 thread: 1,2 s/data; **2 threads: 5,0 s/data** (a cabeça alterna entre arquivos).
  - Ler o arquivo inteiro: 1,46 s; leitor atual: 1,40 s; só os blocos necessários: 1,34 s.
- Conclusão: o leitor atual já está a ~5% do piso físico. As linhas usadas pelo
  overview ficam espalhadas pelo arquivo inteiro, então o disco varre tudo.
- Tentado e **revertido**: thread de pré-leitura separada da descompressão (sem
  ganho; juntar blocos próximos acabava lendo o arquivo inteiro).
- Mantido: HD detectado → 1 leitor sequencial; pinos, ROI e tiles esperam o
  overview terminar no HD.
- Ganho real no HD só mudando o layout dos dados (COG com overviews internos) ou SSD.

### Abertura rápida
- Medição (`tsv --measure-startup`): GDAL 7 ms; janela + OpenGL ~137 ms;
  primeiro quadro ~145 ms. Quase tudo é o driver criando a janela/contexto.
- A leitura dos metadados da série roda em segundo plano: abrir um cubo de 41
  arquivos no HD não atrasa a janela (primeiro quadro de 287 → ~150 ms).
- Regra: nada novo entra no caminho até o primeiro quadro. O Zeit sobe depois,
  em segundo plano.

### Linha de comando: só visualização rápida
- `tsv arquivo/pasta` abre e mostra; opções só para isso (`--band`, memória,
  threads) e para desenvolvimento/testes. Bandas por data, índice (diferença
  normalizada), máscara de qualidade e ferramentas do Zeit ficam **na interface**,
  sem comandos na CLI (decisão de 2026-10-08).

### Integração com o Zeit
- **Processo separado** (servidor Python), não Python embutido nem ligação direta
  do C++ do Zeit:
  - usa o Zeit como ele é (C++ + Python), sem copiar lógica;
  - um erro no Zeit não derruba a interface;
  - dois GDAL (o do tsv e o do rasterio) convivem sem conflito de DLL;
  - o Zeit é GPL-2.0: rodando como processo separado, o tsv continua independente.
- **Runtime Python privado e invisível** dentro da instalação (`runtime\`), com
  o Zeit da wheel do PyPI e só as dependências necessárias (numpy, scipy,
  rasterio, dask, xarray). Sem PyTorch/Earth Engine (somariam GBs).
- **Ponte no repositório do tsv** (não no Zeit), usando só a API pública do Zeit.
  O Zeit continua uma biblioteca pura.
- **Menu gerado pelo servidor**: ele descreve as ferramentas (parâmetros, tipos,
  padrões, ajuda, dados exigidos, saídas) e o tsv monta os formulários. Novo
  algoritmo/parâmetro = editar Python, sem recompilar o tsv.
- Três níveis de execução:
  1. **pixel** (série já na memória → resultado em ~1 ms, modelo desenhado no gráfico);
  2. **ROI / área visível / cena**: o Zeit lê direto dos arquivos (via VRT gerado
     pelo tsv) com os batches C++/OpenMP dele; o tsv só acompanha progresso;
  3. **resultados** voltam como GeoTIFFs no mesmo grid e viram camadas no mapa.
- **Uma ferramenta = um módulo** `zeit_bridge/tool_*.py` (manifesto + `pixel()` +
  `chunk()`); a ponte cuida do protocolo, da leitura em faixas e da escrita dos
  GeoTIFFs. Um módulo com erro não derruba os outros (o erro vai no manifesto).
- Modo desenvolvedor: apontar para um Python próprio com o Zeit editável.
- Medido na 0.4.0: processo do Zeit pronto em ~1,2 s (em segundo plano, depois
  do primeiro quadro); LandTrendr por pixel com ida e volta de 8–16 ms; recorte
  de 512×512 px × 41 anos em ~5 s (incluindo ~1,5 s para o processo subir).
- Runtime: 437 MB (instalador de 122,6 MB). O primeiro carregamento depois de
  instalar levou 15 s (antivírus varrendo ~10 mil arquivos novos); por isso o
  instalador faz um "aquecimento" (`import zeit`) no fim.
- Limite de 260 caracteres de caminho do Windows: `site-packages` virou `sp`
  (caminho relativo mais fundo: 108 caracteres) e o instalador recusa pastas
  com mais de 140 caracteres, com mensagem, antes de copiar.
- Testes sem tocar na tela (`--selftest-zeit`, `--measure-startup`): a interface
  não é testada simulando mouse/teclado enquanto o usuário usa o computador.

### Multiplataforma: Windows, Linux e macOS (Apple Silicon)
- Objetivo: o tsv deve rodar nos três. **Nenhum código de sistema operacional
  entra no código comum**: fica isolado (`src/platform.*`, `ZeitProcess`) e
  listado aqui. Dependências novas devem ser multiplataforma.
- Situação (0.11.0): **Windows** e **Linux x86_64** compilados e testados
  (Linux no WSL, Ubuntu 24.04, com os mesmos autotestes); **macOS** (Apple
  Silicon, macOS 26) compilado da fonte com GDAL do conda, autotestes de UI e do
  Zeit passando; falta o bundle `.app`.

| Onde | Windows | Linux | macOS |
|---|---|---|---|
| `src/platform.cpp` | IFileOpenDialog, IOCTL de seek penalty, Explorer | `zenity`/`kdialog`, `/sys/dev/block/*/queue/rotational`, `xdg-open`, `~/.local/share/tsv` | AppleScript, `open`, `~/Library/Application Support/tsv` (não testado) |
| `ZeitProcess` (`src/zeit_client.cpp`) | CreateProcess + pipes, lista de handles herdados | `posix_spawn` + pipes (testado) | o mesmo (testado) |
| `tsv.com` (`src/launcher.cpp`) | truque do `.com` para o console | desnecessário: um binário só | idem |
| runtime do Zeit | Python *embeddable* | `python-build-standalone` *stripped* + wheels manylinux, `strip --strip-debug` (638 MB) | receita `macos_arm64`: wheels até `macosx_15_0`, `strip -S` seguido de `codesign --force --sign -` (o strip invalida a assinatura e o kernel mata o processo) (446 MB) |
| bibliotecas | `deploy_runtime.cmake` (dumpbin) | `package_linux.cmake`: `GET_RUNTIME_DEPENDENCIES`, RPATH `$ORIGIN/lib`; glibc, OpenGL e X11 vêm do sistema | bundle `.app` (a fazer) |
| pacote | Inno Setup (122 MB) | `.tar.xz` portátil (172 MB) com `tsv.desktop`; AppImage/.deb depois | `.app` + `.dmg` assinado (a fazer) |
| OpenGL 3.3 core | ok | ok (WSLg/Mesa) | exige `GLFW_OPENGL_FORWARD_COMPAT` (já definido); OpenGL obsoleto lá, o ImGui tem backend Metal |

## Roadmap

| Fase | Versão | Conteúdo | Status |
|---|---|---|---|
| 0 | 0.3.0 | Migração para `tsv`, tudo em inglês, README, IDEIAS.md, abertura medida e otimizada | concluída |
| 1 | 0.4.0 | Runtime Python embutido, ponte do Zeit, menu de ferramentas, tarefas, resultados como camadas, **LandTrendr** (pixel + raster), zoom limitado ao extent | concluída |
| 2 | 0.5.0 | **Espaço de trabalho**: painel **Layers** sempre ativo (todas as camadas, inclusive o raster inicial, ligar/desligar), **várias séries abertas ao mesmo tempo** com o gráfico mostrando todas ou uma, **painéis destacáveis** para outros monitores, **árvore de arquivos** | concluída |
| 3 | 0.6.0 | **Mann-Kendall** (variantes do Zeit) e **BFAST / BFAST Lite / BFAST Monitor** | concluída |
| 4 | 0.7.0 | Cubo com várias bandas por data + máscara de qualidade; **fenologia** e **CCDC** | concluída |
| 5 | 0.8.0 | **Ferramentas mais fáceis de usar**: estimativa de tempo antes de rodar (medida pelo Zeit numa amostra), ajuste do Zeit nas séries de **todas as camadas** | concluída |
| 6 | 0.9.0 | Mais do Zeit: **suavização** (Whittaker/Savitzky-Golay) no gráfico, **TWDTW** (classificação por padrões tirados dos pinos) | concluída |
| 7 | 0.10.0 | **Linux**: compilar e testar (ambiente conda-forge), processos POSIX para o Zeit, runtime com `python-build-standalone`, pacote `.tar.xz` portátil | concluída |
| 8 | 0.11.0 | **Dados categóricos** (detecção, cores e nomes de classe, legenda, gráfico em degraus, estatísticas de classe); Mann-Kendall fora da tabela de estatísticas | concluída |
| 9 | 0.12.0 | **Reprojeção** de camadas com CRS diferente (grade de warp na GPU) e **ROI em todas as camadas** | próxima |
| 10 | 0.13.0 | **Novas visualizações**: transecto espaço-tempo (Hovmöller), mapa de calor ano × dia do ano, cortina (swipe) entre datas/camadas, área por classe ao longo do tempo e matriz de transição (categóricos) | planejada |

## Ideias (backlog)

### Análise
- Mapa de **quebra**: ano e magnitude da maior queda por pixel (barato na GPU).
- ~~Mapa de **tendência significativa**~~ — feito na 0.6.0 (Mann-Kendall do Zeit,
  mapa "Significant Sen's slope").
- ~~**Estimativa de tempo**~~ — feita na 0.8.0 (ver histórico). Ideia original: estimar antes de rodar uma ferramenta lenta (BFAST ~2–3 ms/pixel:
  uma cena Landsat inteira levaria horas). Ideia: o manifesto declara um custo por
  pixel medido e a janela mostra a estimativa para o escopo escolhido.
- BFAST: desenhar o **modelo ajustado** (tendência + sazonalidade) no gráfico —
  o Zeit hoje devolve só as quebras.
- BFAST Monitor: história estável (o R corta a parte instável da história; o
  Zeit usa a história inteira, o que gera alarmes falsos após uma quebra antiga).
- Mapa de **diferença** entre duas datas (Δ).
- **Boxplot por data** da ROI e histograma dos valores da série.
- **Suavização** opcional da série (média móvel, Savitzky-Golay).
- Para séries intra-anuais: **um ano sobre o outro** (eixo = dia do ano),
  climatologia (média ± desvio por mês).

### Dados e desempenho
- **Cache em resolução total**, em blocos com o tempo contíguo (estilo Zarr) num
  SSD: série exata e ROI em ~1 ms mesmo com os dados num HD.
- Ferramenta "**otimizar dados**": converter para COG com overviews internos
  (estimativa: 1ª abertura 3–10× mais rápida no HD; não medido).
- NetCDF com dimensão de tempo; reprojeção; paletas por classe (color table).

### Interface
- **Visualizações sugeridas** (2026-10-08, ordem de prioridade): transecto
  espaço-tempo (linha no mapa → imagem distância × data); mapa de calor ano ×
  dia do ano/mês da série; cortina (swipe) entre duas datas ou camadas;
  dispersão entre camadas/datas na ROI; para categóricos, área por classe ao
  longo do tempo e matriz de transição entre duas datas. → fase 0.13.0.
- ~~Painel Layers, várias séries ao mesmo tempo, painéis destacáveis, árvore de
  arquivos~~ — feitos na 0.5.0 (detalhes no histórico).
- Camadas com **CRS diferentes**: hoje só aparecem quando ativas; reprojetar o
  overview (GDAL warp) permitiria sobrepor qualquer par.
- **ROI em todas as camadas** (hoje só na ativa). ~~Zeit nas outras camadas~~ —
  feito na 0.8.0 (ferramentas de uma banda; as multibanda só na ativa).
- Árvore de arquivos: mostrar as datas reconhecidas e quantos arquivos formam a
  série antes de abrir; favoritos.
- Arrastar e soltar com Shift para **adicionar** como camada (hoje substitui).
- **Ordem das abas laterais**: Layers/Files primeiro, depois Display; o painel
  **Performance desligado por padrão**, ligado por View → Performance para quem
  quiser (pedido em 2026-10-08; feito na 0.7.0).
- ~~Zoom out limitado ao extent~~ — feito na 0.4.0.

### Produto
- "Atualizar Zeit" dentro do app (baixa a wheel nova para o runtime privado).
- Ferramentas de IA do Zeit como download opcional (PyTorch é pesado).
- Releases no GitHub com o instalador anexado.
- Linux (os stubs de `platform.cpp` existem; falta testar).
- Escala de interface (DPI) e fonte TTF para telas 4K.

## Histórico

### 0.11.0 — Fase 8: dados categóricos
- **Detecção** ao abrir: tabela de cores, nomes de categoria ou tabela de
  atributos (RAT) na banda → categórico; senão, se a primeira data lida tiver
  só valores inteiros e no máximo 40 distintos. Séries contínuas (NDVI etc.)
  ficam como estão. O painel Display liga/desliga à mão (até 256 classes).
- **Cores e nomes**: os do arquivo quando existem; senão uma paleta qualitativa
  (Tableau 20), sem repetir cor quando uma classe só aparece em datas
  posteriores. Legenda editável (cor, nome, mostrar/ocultar a classe) com a
  participação de cada classe. No shader, uma tabela (LUT) valor → cor de 4096
  posições; a amostragem já era "nearest", então as bordas não misturam códigos.
- **Gráfico** em degraus com os nomes das classes no eixo Y; sem tendência nem
  média da ROI (média de códigos não faz sentido). **Estatísticas** de classe:
  classe na data, classe majoritária (% das datas), classes vistas, número de
  mudanças, última mudança ("2006: Forest → Pasture"). Status bar com o nome.
- Só o modo "valor na data" para séries categóricas.
- **Mann-Kendall saiu da tabela de estatísticas** (Z, p-valor e "tendência"):
  o teste completo, com correções de autocorrelação, é a ferramenta do Zeit. A
  inclinação de Sen continua (barata e útil no gráfico).
- `--selftest-ui A B C D E`: D = série categórica com tabela de cores (uma
  imagem por ano, como o MapBiomas), E = mesma série sem tabela; confere
  detecção, nomes/cores, cor do pixel no mapa, cores distintas e o resumo.
- Reprojeção e ROI em todas as camadas passaram para a 0.12.0.

### 0.10.0 — Fase 7: Linux
- Compila no Linux com um ambiente conda-forge (`tsv-linux`: compilador, GDAL,
  cabeçalhos X11/OpenGL), sem instalar nada no sistema. Só um ajuste no código
  comum: `GetMetadata` do GDAL 3.13 devolve `CSLConstList`.
- **`ZeitProcess` POSIX** (`posix_spawn`, pipes com close-on-exec, ambiente
  filtrado como no Windows, stderr no log, SIGKILL para cancelar).
- **`platform` no Linux/macOS**: diálogos via `zenity`/`kdialog` (macOS:
  AppleScript), abrir pastas (`xdg-open`/`open`), detecção de HD por
  `/sys/dev/block`, pasta de dados XDG.
- **Runtime do Zeit no Linux**: `python-build-standalone` 3.12.10 (*stripped*,
  sha256 do `SHA256SUMS` oficial) + wheels manylinux; `strip --strip-debug` nas
  bibliotecas nativas tirou 153 MB (llvmlite e o módulo do Zeit vêm com
  símbolos de depuração). 949 → 638 MB.
- **Pacote portátil** `tsv-<versão>-linux-x86_64.tar.xz` (172 MB): 64
  bibliotecas em `lib/`, RPATH `$ORIGIN/lib`, dados do PROJ/GDAL, runtime,
  `tsv.desktop`. Testado num ambiente limpo (`env -i`, sem conda): `ldd` só
  aponta glibc/OpenGL/X11 do sistema; `--selftest-ui` e `--selftest-zeit` (9
  ferramentas) passam. No Linux o Zeit sobe em ~0,75 s e as tarefas rodam ~2×
  mais rápido que no Windows na mesma máquina.

### 0.9.0 — Fase 6: classificação e suavização
- **TWDTW** (Zeit, feito por um sub-agente): classificação de cada pixel pelo
  padrão mais parecido. Os **padrões vêm dos pinos** (ou da média da ROI) e
  recebem nome de classe na janela da ferramenta (novo tipo de parâmetro
  `patterns`). Mapas: classe (legenda com as cores no painel Layers, nome da
  classe na barra de status), distância e margem para a 2ª classe. Datas em
  dias absolutos (o padrão casa com o mesmo período, não com o dia do ano);
  lacunas da série interpoladas. Em série sintética: 100% dos pixels certos.
- **Suavização** (Zeit: Whittaker e Savitzky-Golay), só no gráfico (ferramenta
  sem saídas raster). Ambas usam o índice das observações, não as datas.
- Mapas de classes: saída com `classes_param` → o resultado traz os nomes; cada
  classe ganha uma cor de paleta qualitativa (Dark, ou Paired com mais de 8).
- `localtime_s` (só Windows) saiu do código comum: `platform::localTime`.
- STL **não** entrou: o Zeit só tem STL dentro do BFAST (C++), sem API Python.
  Expor isso seria mudança no Zeit.
- Ideias do sub-agente para depois: TWDTW "por estação" (padrão de um ano casado
  em todos os anos, como no pacote R) e corte por classe (`abort_threshold` por
  pixel, hoje um escalar no Zeit).

### 0.8.0 — Fase 5: ferramentas mais fáceis de usar
- **Estimativa de tempo** na janela de cada ferramenta, para o escopo escolhido
  (imagem inteira, área visível, ROI). A ponte (`estimate`) roda a ferramenta em
  amostras do centro da série (8, 16 e, quando barato, 64 e 128 px de lado,
  depois de uma chamada de aquecimento) e ajusta *custo fixo por bloco + custo
  por pixel*: o LandTrendr gasta ~1 s por chamada qualquer que seja o tamanho
  (até 200×200 px), então medir só por pixel errava 100×. Medido contra a
  execução real (só o cálculo; leitura e início do processo à parte): BFAST
  6,0 s vs 6,0 s, BFAST Lite 37,7 s vs 37,8 s, fenologia 49 s vs 38 s,
  LandTrendr 1,1 s vs ~1,5 s.
- **Zeit nas outras camadas**: com "All visible layers", a ferramenta do gráfico
  também é ajustada no cursor e nos pinos das outras camadas visíveis (com as
  datas de cada uma); números na tabela de estatísticas.
- Leitura das entradas da ponte reorganizada (`Inputs`), usada pelas tarefas e
  pela estimativa; `--selftest-zeit` compara estimativa e execução.

### 0.7.0 — Fase 4: várias bandas por data, fenologia e CCDC
- **Cubo com várias bandas por data** (um arquivo por data, ex.: reflectância
  Landsat): painel Display → Bands escolhe a banda mostrada ou a **diferença
  normalizada** de duas (NDVI, NDMI, NBR) e a **banda de qualidade** (códigos
  Fmask, bits do `QA_PIXEL` do Landsat Coleção 2 ou máscara 0/1). Uma banda
  chamada Fmask/QA_PIXEL é usada automaticamente. Aplicar reabre a camada **no
  lugar** (posição, nome, pinos, vista e resultados mantidos). O cache do
  overview só muda de chave quando há índice/máscara (caches antigos continuam
  válidos).
- **Fenologia** (Zeit, feita por um sub-agente): início/pico/fim da estação,
  duração, pico, amplitude, R²; mapas para o ano típico (mediana), a estação
  mais recente ou um ano escolhido; no gráfico, linhas no início/fim e marcador
  no pico. Erro de SOS/EOS ~0–1 dia em dados sintéticos (curva Elmore).
- **CCDC** (Zeit, feito por um sub-agente): primeira ferramenta multibanda. O
  protocolo da ponte ganhou bandas por papel (blue…swir2, thermal), códigos
  Fmask (convertidos da regra de qualidade), datas reais e o que a camada mostra
  (`shown`); o tsv lê as bandas do pixel em segundo plano e escreve um VRT por
  banda para as tarefas. O modelo de cada segmento é desenhado no gráfico na
  unidade mostrada (banda ou índice). Em dados sintéticos com nuvens: quebra na
  primeira observação limpa após a mudança em 100% dos pixels alterados, nenhuma
  nos estáveis; ~0,1 ms/pixel em todos os núcleos.
- Painel **Performance** desligado por padrão (View → Performance); abas da
  esquerda: Layers/Files em cima, Display embaixo (layout `layout-0.7.ini`).
- CLI continua só para visualização rápida (decisão registrada acima).
- `--selftest-ui A B C`: abre uma pasta multibanda, confere a máscara automática
  e reabre como NDVI no lugar. `--selftest-zeit` numa pasta multibanda roda as 7
  ferramentas aplicáveis (incl. CCDC com bandas e QA).
- Notas do Zeit para o futuro (relatadas pelos sub-agentes): a fenologia não
  devolve a curva ajustada e tem constantes fixas para 23 obs/ano; o batch do
  CCDC não devolve magnitude/probabilidade (a ferramenta as deriva dos
  coeficientes); suavizadores não tratam NaN.

### 0.6.0 — Fase 3: tendência e quebras
- **Mann-Kendall** (Zeit): original, Hamed-Rao, Yue-Wang (corrigem a
  autocorrelação, comum em composições anuais) e sazonal (Hirsch & Slack, para
  séries intra-anuais). No gráfico: linha de Sen. Mapas: inclinação de Sen só onde
  a tendência é significativa, inclinação, p, Z, tau, classe, intercepto.
- **BFAST**, **BFAST Lite** e **BFAST Monitor** (Zeit), feitos por um sub-agente
  num módulo próprio. No gráfico: **linhas verticais** nas datas de quebra (novo
  tipo de sobreposição `vlines`) e no início do monitoramento. Mapas: número de
  quebras, data e magnitude da maior, primeira quebra / quebra no monitoramento.
  Notas: os índices de quebra do Zeit são posições na série completa (o
  comentário do cabeçalho diz o contrário); a magnitude do BFAST Lite é a
  diferença das médias dos segmentos (o Zeit não a devolve); as versões de um
  pixel do Zeit fixam `min_valid`, então o modo pixel chama o batch com 1 pixel.
- Ponte dividida em núcleo + **módulos por ferramenta** (`tool_*.py`).
- Requisitos novos no manifesto: série **regular** (datas igualmente espaçadas;
  data faltante = banda sem dado) e **mínimo de observações por ano**; o tsv
  desabilita a ferramenta com o motivo.
- `--selftest-zeit` roda **todas** as ferramentas aplicáveis à série (pixel +
  recorte de 256×256) e confere as saídas. Medido (20 núcleos): série anual
  300×200×41 — LandTrendr 3,6 s, Mann-Kendall 1,4 s; série mensal 256×200×144 —
  Mann-Kendall 1,9 s, BFAST Monitor 1,5 s, BFAST 37 s, BFAST Lite 86 s. Na série
  sintética com queda em 2016-07, BFAST e BFAST Lite acham a quebra em 2016-06
  (última observação antes da queda, convenção do R) em >99% dos pixels.
- Os scripts da ponte são copiados por um alvo próprio do CMake (antes um
  POST_BUILD que só rodava quando o executável era religado).

### 0.5.0 — Fase 2: espaço de trabalho
- **Camadas**: várias séries abertas ao mesmo tempo (File → Add layer, `Ctrl+L`,
  painel Files). O mapa usa a grade da camada **ativa**; as outras são posicionadas
  pelo georreferenciamento (mesmo CRS, sem rotação) — camadas incompatíveis
  aparecem só quando ativas, com o motivo no painel. Cada camada guarda o próprio
  modo, paleta, faixa e data; a data das outras acompanha a da ativa (a mais
  próxima). Trocar a camada ativa preserva a vista geográfica e remapeia os pinos.
- **Painel Layers** sempre visível: ligar/desligar (inclusive a primeira série),
  ordem, opacidade, fechar; resultados do Zeit listados sob a sua série.
- **Gráfico com várias camadas**: "Active layer" ou "All visible layers" (cursor e
  pinos de cada camada, cada uma com o seu formato de marcador e as suas datas);
  colunas correspondentes na tabela de estatísticas.
- **Painel Files**: árvore de pastas listada em segundo plano, só rasters por
  padrão, recentes, abrir/adicionar como camada, seleção múltipla.
- **Painéis destacáveis** para outros monitores (multi-viewports do Dear ImGui).
- Painel "Layer" renomeado para **Display** (configurações da camada ativa); novo
  layout padrão (arquivo `layout-0.5.ini`).
- `--selftest-ui A B`: percorre o fluxo de camadas numa **janela invisível** e
  confere alinhamento, séries das duas camadas, pixels do mapa (camada visível,
  oculta, todas ocultas), troca da ativa e fechamento — sem mexer no mouse.
- Flag do OpenGL para macOS e seção de portabilidade neste arquivo.

### 0.4.0 — Fase 1: Zeit no tsv
- **Runtime Python privado** (`runtime\`): Python 3.12 embeddable + Zeit 0.25.0
  (wheel do PyPI, `--no-deps`) + dependências fixadas, sem PyTorch; montado por
  `tools/build_zeit_runtime.py` (testes removidos: −102 MB).
- **Ponte** `zeit_bridge/tsv_zeit_bridge.py` no repositório do tsv (o Zeit não foi
  alterado): `serve` (lista de ferramentas + chamadas por pixel, JSON por linha)
  e `job` (processo descartável por execução em raster, com progresso; cancelar
  = encerrar o processo).
- tsv: cliente com processos sem janela, herdando só os handles necessários e
  com ambiente limpo; inicia o Zeit em segundo plano ao abrir uma série.
- Menu **Tools** gerado da lista enviada pela ponte; janela da ferramenta com
  formulário, checagem de aplicabilidade, ajuste no gráfico e execução no raster
  (imagem inteira, área visível ou ROI).
- **LandTrendr**: segmentos desenhados sobre as séries do cursor, pinos e ROI, com
  as linhas do modelo na tabela de estatísticas; no raster, 7 mapas (ano, magnitude,
  duração, pré/pós, taxa, DSNR) carregados como camadas por cima do mapa, com
  paleta, faixa e opacidade próprias.
- Painel **Tasks** (progresso, cancelar, abrir pasta, log do Zeit).
- **Zoom out limitado ao extent** da imagem.
- `--selftest-zeit` (caminho completo sem janela) e opções de desenvolvedor
  `--zeit-python` / `--zeit-bridge`.
- Instalador com o runtime (122,6 MB), aquecimento do Zeit e checagem de pasta
  longa; testado instalando e rodando sem Python/conda/GDAL no PATH.

### 0.3.0 — Fase 0: tsv
- Projeto migrado de `rs_gui` para **tsv** (repositório `sacridini/tsv`).
- Interface, CLI, instalador e código-fonte traduzidos para o inglês.
- README completo; este IDEIAS.md.
- Abertura medida (`--measure-startup`) e otimizada: leitura de metadados em
  segundo plano (primeiro quadro ~150 ms mesmo abrindo um cubo).
- Tendência da ROI só é desenhada com a ROI completa; ROI calculada em ordem cronológica.

### 0.2.x — rs_gui (antes da migração)
- Cubo na GPU (overview em array de texturas) com cache em disco.
- Estatísticas temporais por pixel em shader; 10 modos de mapa; paletas; histograma.
- Tiles de detalhe em resolução real com cache LRU na GPU.
- Série exata no cursor (cancelável), pinos, ROI com p10–p90.
- Estatísticas: OLS, Sen's slope, Mann-Kendall; CSV.
- Gráfico com estilos (linhas, pontos, hastes, degraus), anomalia, z-score, tendência OLS/Sen.
- Detecção de HD; leitura sequencial; adiamento de leituras aleatórias.
- Instalador (Inno Setup), ícone, lançador de console, DLLs do GDAL empacotadas.
