# IDEIAS — Janus

Registro de ideias, decisões (com os números que as motivaram) e avanços do
projeto. Atualizado a cada fase.

## Visão

Um visualizador **especializado e muito rápido** para séries temporais raster e
cubos de dados: abrir, navegar no tempo, inspecionar a série de qualquer pixel e
rodar análises de mudança/tendência — sem ser um SIG completo. Instalável no
Windows sem Python nem conda, e chamável pela linha de comando
(`jn serie.tif`).

## Decisões (e por quê)

### Dear ImGui + OpenGL, não uma GUI própria em Vulkan
- O trabalho de GPU é mínimo: estatísticas temporais de 5,5 Mpx × 41 datas em
  ~90 ms (uma vez); um quadro custa 0,2–0,4 ms de CPU.
- O gargalo é disco/descompressão, que Vulkan não resolve. Reescrever texto,
  widgets, docking e gráficos custaria meses sem ganho perceptível.
- O ImGui tem backends Vulkan e Metal, e o código de GPU está isolado atrás de
  `gpu.hpp` e `render_backend.hpp`: no macOS ele já roda em Metal (OpenGL é
  obsoleto lá); Windows e Linux continuam em OpenGL 3.3.

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
- Medido de novo em 2026-10-08 (série 215_066, 41 datas de 7441×7317, Float32
  LZW em faixas de 1 linha, HD Seagate ST4000DM004 de 5400 rpm): o Gerenciador
  de Tarefas mostra só ~61 MB/s e ~66% de uso, mas o leitor pede 1 de cada ~3
  faixas (overview 1:3,15) e o disco passa por cima das outras: 61 × 3 ≈ 180
  MB/s, o teto do disco. ~45 s a frio; ~28 s com os arquivos já na RAM (aí o
  limite é a CPU de 1 leitor).
- **COG medido** (mesma série convertida, DEFLATE + preditor, blocos 512,
  overviews NEAREST): pixels idênticos; 8,5 → 9,0 GB; 4,4 min para converter.
  O Janus pede 1,6 GB em vez de 2,6 GB e leva 11 s em vez de 28 s (arquivos na
  RAM). Ganho menor que o estimado porque o overview do Janus (1:3,15) cai
  entre os níveis do COG (1:2 e 1:4) e o GDAL lê o 1:2. Usar o nível 1:4 (≈400
  MB por série, poucos segundos) exigiria um overview de menos resolução.
  Decisão: os dados ficam como estão; nada de conversão por enquanto.
- **Cache do overview gravado data a data** (índice das datas prontas depois do
  cabeçalho): fechar o Janus no meio da leitura não perde nada; a próxima
  abertura lê o que já existe e só busca no HD as datas que faltam (testado:
  morto aos 15 s com 14/41 datas; reaberto, leu só as 27 restantes em ~28 s).
  Caches do formato anterior (completos, sem índice) continuam válidos.

### Abertura rápida
- Medição (`jn --measure-startup`): GDAL 7 ms; janela + OpenGL ~137 ms;
  primeiro quadro ~145 ms. Quase tudo é o driver criando a janela/contexto.
- A leitura dos metadados da série roda em segundo plano: abrir um cubo de 41
  arquivos no HD não atrasa a janela (primeiro quadro de 287 → ~150 ms).
- Regra: nada novo entra no caminho até o primeiro quadro. O Zeit sobe depois,
  em segundo plano.

### Linha de comando: só visualização rápida
- `jn arquivo/pasta` abre e mostra; opções só para isso (`--band`, memória,
  threads) e para desenvolvimento/testes. Bandas por data, índice (diferença
  normalizada), máscara de qualidade e ferramentas do Zeit ficam **na interface**,
  sem comandos na CLI (decisão de 2026-10-08).

### Integração com o Zeit
- **Processo separado** (servidor Python), não Python embutido nem ligação direta
  do C++ do Zeit:
  - usa o Zeit como ele é (C++ + Python), sem copiar lógica;
  - um erro no Zeit não derruba a interface;
  - dois GDAL (o do Janus e o do rasterio) convivem sem conflito de DLL;
  - o Zeit é GPL-2.0: rodando como processo separado, o Janus continua independente.
- **Runtime Python privado e invisível** dentro da instalação (`runtime\`), com
  o Zeit da wheel do PyPI e só as dependências necessárias (numpy, scipy,
  rasterio, dask, xarray). Sem PyTorch/Earth Engine (somariam GBs).
- **Ponte no repositório do Janus** (não no Zeit), usando só a API pública do Zeit.
  O Zeit continua uma biblioteca pura.
- **Menu gerado pelo servidor**: ele descreve as ferramentas (parâmetros, tipos,
  padrões, ajuda, dados exigidos, saídas) e o Janus monta os formulários. Novo
  algoritmo/parâmetro = editar Python, sem recompilar o Janus.
- Três níveis de execução:
  1. **pixel** (série já na memória → resultado em ~1 ms, modelo desenhado no gráfico);
  2. **ROI / área visível / cena**: o Zeit lê direto dos arquivos (via VRT gerado
     pelo Janus) com os batches C++/OpenMP dele; o Janus só acompanha progresso;
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
- Objetivo: o Janus deve rodar nos três. **Nenhum código de sistema operacional
  entra no código comum**: fica isolado (`src/platform.*`, `ZeitProcess`) e
  listado aqui. Dependências novas devem ser multiplataforma.
- Situação (0.11.0): **Windows** e **Linux x86_64** compilados e testados
  (Linux no WSL, Ubuntu 24.04, com os mesmos autotestes); **macOS** (Apple
  Silicon, macOS 26) compilado da fonte com GDAL do conda, autotestes de UI e do
  Zeit passando; falta o bundle `.app`.

| Onde | Windows | Linux | macOS |
|---|---|---|---|
| `src/platform.cpp` | IFileOpenDialog, IOCTL de seek penalty, Explorer | `zenity`/`kdialog`, `/sys/dev/block/*/queue/rotational`, `xdg-open`, `~/.local/share/janus` | AppleScript, `open`, `~/Library/Application Support/Janus`, cache em `~/Library/Caches/Janus` (fora do Time Machine) (diálogos não testados) |
| `ZeitProcess` (`src/zeit_client.cpp`) | CreateProcess + pipes, lista de handles herdados | `posix_spawn` + pipes (testado) | o mesmo (testado) |
| `jn.com` (`src/launcher.cpp`) | truque do `.com` para o console | desnecessário: um binário só | idem |
| runtime do Zeit | Python *embeddable* | `python-build-standalone` *stripped* + wheels manylinux, `strip --strip-debug` (638 MB) | receita `macos_arm64`: wheels até `macosx_15_0`, `strip -S` seguido de `codesign --force --sign -` (o strip invalida a assinatura e o kernel mata o processo) (446 MB) |
| bibliotecas | `deploy_runtime.cmake` (dumpbin) | `package_linux.cmake`: `GET_RUNTIME_DEPENDENCIES`, RPATH `$ORIGIN/lib`; glibc, OpenGL e X11 vêm do sistema | `package_macos.cmake`: `GET_RUNTIME_DEPENDENCIES` para `Contents/Frameworks` (as libs do conda já usam `@rpath` + `@loader_path/`), RPATH `@executable_path/../Frameworks`, dados e runtime em `Contents/Resources` (`platform::resourceDir`) |
| pacote | Inno Setup (122 MB) | `.tar.xz` portátil (172 MB) com `janus.desktop`; AppImage/.deb depois | `Janus.app` em `.dmg` (199 MB), assinatura ad hoc; Developer ID + notarização quando houver conta Apple Developer; "Abrir com" do Finder via `application:openURLs:` acrescentado ao delegate do GLFW |
| CI | GitHub Actions: build, `--selftest-zeit`, instalador instalado em silêncio e testado | build, autotestes de UI (Xvfb + Mesa) e do Zeit, pacote extraído e testado | build, `--selftest-zeit`, app do `.dmg` testado (os runners não têm Metal utilizável para o teste de UI) |
| renderer (`gpu_*`, `render_backend_*`) | OpenGL 3.3 core | OpenGL 3.3 core (WSLg/Mesa) | **Metal** (`gpu_metal.mm`, `render_backend_metal.mm`): o buffer do cubo é o próprio array do `Overview` (alinhado à página, `newBufferWithBytesNoCopy`: nada é enviado, o cubo existe uma vez na RAM e o `--budget` é o total), estatísticas em buffers compartilhados lidos pela CPU sem cópia, MSL compilado na abertura, `CAMetalLayer` na resolução Retina; `-DJANUS_RENDERER=GL` mantém o caminho OpenGL para comparar. Medido (cubo de 1 GB, M4, Release): pico de 1,2 GB com cache (GL: 3,8 GB; Metal com cópia: 2,2 GB), 2,0 GB a frio (GL: 3,8 GB), carga 0,2 s com cache e 0,6 s a frio (GL: 0,4 s e 0,8 s), primeiro quadro ~90 ms contra ~115 ms; pixels idênticos nos 10 modos |
| gestos (`platform::takeGestures`) | roda = zoom | roda = zoom | `platform_mac.mm`: monitor local do `NSEvent`; pinça = zoom, rolagem com fases de gesto (trackpad, Magic Mouse) = mover; roda de mouse = zoom, mesmo com rolagem suave (deltas precisos, ex.: Logitech), que não tem fases |
| densidade da tela | 1 pixel por ponto | idem | Retina: 2 pixels por ponto; o mapa é desenhado em `FramebufferScale` do viewport onde está, e o nível dos tiles é escolhido por pixel de tela |

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
| 9 | 0.22.0 | **Reprojeção** de camadas com CRS diferente (grade de warp na GPU) e **ROI em todas as camadas** | concluída |
| — | 0.16.0 | **Mapas lado a lado**: painéis de mapa extras (View → New map view), cada um com uma camada e, se quiser, data e modo próprios; todos na mesma área (compartilham `scale_`/`offset_`, cada canvas centrado como o principal), cursor espelhado como cruz, pinos e ROI em todos. Um alvo de desenho por painel (`Gpu::beginMap(..., slot)`); `TileManager::tick()` uma vez por quadro, para que vários painéis pedindo tiles da mesma camada não descartem os pedidos uns dos outros | concluída |
| — | 0.17.0 | **Janus**: o tsv passa a se chamar Janus (comando `jn`; repositório `sacridini/janus`); pastas de dados e cache migradas na primeira abertura, o instalador do Windows remove um tsv instalado | concluída |
| 10 | 0.20.0 | **Novas visualizações**: **cortina (swipe)** entre datas/camadas e **transecto espaço-tempo (Hovmöller)** | concluída |
| 11 | 0.19.0 | **Exportação**: mapa visível como PNG (figuras), vista/camada e resultados do Zeit como GeoTIFF (para o QGIS) | concluída |
| 12 | 0.18.0 | **Mapas de diferença (Δ) e de quebra** (ano e magnitude da maior queda), calculados na GPU | concluída |
| 13 | 0.21.0 | **Cache em resolução total** num SSD (blocos de 64×64 por data, compressão sem perda): série exata e ROI em ~1 ms mesmo com os dados num HD | concluída |
| 15 | 0.23.0 | **Mapa de fundo (basemap)**: imagem de satélite/mapa da web por baixo das séries (Esri World Imagery, Sentinel-2 cloudless, OSM, URL XYZ própria), pelo driver WMS/TMS do GDAL, reprojetado pela grade de warp | concluída |
| 16 | — | **Configurações**: janela própria (File → Settings); **limite de núcleos** para o processamento do Janus e do Zeit (padrão: todos menos 2), **tema da interface** (escuro, claro, clássico, Janus) e as opções que hoje estão no painel Performance | em andamento |
| 14 | — | Mais visualizações: mapa de calor ano × dia do ano, área por classe ao longo do tempo e matriz de transição (categóricos), dispersão entre camadas na ROI | planejada |

Ordem decidida em 2026-10-08: as fases 10–13 em paralelo (um sub-agente por
fase, cada um num worktree; o merge, os testes e a versão são feitos fase a
fase), depois a 9, que mexe em quase tudo. A versão é atribuída quando a fase
termina.

## Ideias (backlog)

### Análise
- Mapa de **quebra**: ano e magnitude da maior queda por pixel (barato na GPU) → fase 12.
- ~~Mapa de **tendência significativa**~~ — feito na 0.6.0 (Mann-Kendall do Zeit,
  mapa "Significant Sen's slope").
- ~~**Estimativa de tempo**~~ — feita na 0.8.0 (ver histórico). Ideia original: estimar antes de rodar uma ferramenta lenta (BFAST ~2–3 ms/pixel:
  uma cena Landsat inteira levaria horas). Ideia: o manifesto declara um custo por
  pixel medido e a janela mostra a estimativa para o escopo escolhido.
- BFAST: desenhar o **modelo ajustado** (tendência + sazonalidade) no gráfico —
  o Zeit hoje devolve só as quebras.
- BFAST Monitor: história estável (o R corta a parte instável da história; o
  Zeit usa a história inteira, o que gera alarmes falsos após uma quebra antiga).
- Mapa de **diferença** entre duas datas (Δ) → fase 12.
- **Boxplot por data** da ROI e histograma dos valores da série.
- **Suavização** opcional da série (média móvel, Savitzky-Golay).
- Para séries intra-anuais: **um ano sobre o outro** (eixo = dia do ano),
  climatologia (média ± desvio por mês).

### Dados e desempenho
- **Cache em resolução total**, em blocos com o tempo contíguo (estilo Zarr) num
  SSD: série exata e ROI em ~1 ms mesmo com os dados num HD → fase 13.
- Ferramenta "**otimizar dados**": converter para COG com overviews internos.
  Medido em 2026-10-08: ~2,5× mais rápido com os arquivos na RAM (ver
  "Leitura em HD mecânico"); deixado de lado por ora. Se voltar: escolher o
  tamanho do overview alinhado aos níveis do COG.
- **Exportação** (hoje só "Copy CSV"): mapa visível em PNG, vista e resultados
  em GeoTIFF → fase 11.
- **Mapa de fundo** (pedido em 2026-10-08) → fase 15. Sem dependência nova: o
  GDAL embutido tem o driver WMS/TMS e a `libcurl` já vai no instalador.
  Testado: recorte de ~22×22 km da Esri World Imagery em 1024×1024 px em ~2,3 s
  da internet e ~1,2 s do cache em disco do GDAL. Os tiles estão em EPSG:3857 e
  passam pela grade de warp da 0.22.0. Desligado por padrão; atribuição no
  mapa e no PNG exportado. Google Satellite só por URL própria com chave
  (os termos proíbem usar os tiles direto).
- NetCDF com dimensão de tempo; reprojeção; paletas por classe (color table).

### Interface
- **Visualizações sugeridas** (2026-10-08, ordem de prioridade): transecto
  espaço-tempo (linha no mapa → imagem distância × data); mapa de calor ano ×
  dia do ano/mês da série; cortina (swipe) entre duas datas ou camadas;
  dispersão entre camadas/datas na ROI; para categóricos, área por classe ao
  longo do tempo e matriz de transição entre duas datas. → cortina e
  transecto na fase 10; o resto na fase 14.
- ~~Painel Layers, várias séries ao mesmo tempo, painéis destacáveis, árvore de
  arquivos~~ — feitos na 0.5.0 (detalhes no histórico).
- ~~Camadas com **CRS diferentes**~~ — feito na 0.22.0 (grade de warp na GPU).
- ~~**ROI em todas as camadas**~~ — feito na 0.22.0. ~~Zeit nas outras camadas~~ —
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

### 0.23.0 — Fase 15: mapa de fundo
- Fontes: **Esri World Imagery** (até z18), **Sentinel-2 cloudless 2016**
  (EOX; o único ano em CC BY 4.0, os de 2017 em diante são CC BY-NC-SA),
  **OpenStreetMap** (2 conexões, User-Agent do Janus, cache de 7 dias, como
  pede a política de uso) ou **URL XYZ própria** (`{z}/{x}/{y}`, `{-y}`, `{s}`,
  zoom máximo, tiles de 256/512, atribuição digitada). Sem Google pronto.
- Pelo driver WMS/TMS do GDAL (curl + cache em disco de 512 MB em
  `cache/basemap`; sem dependência nova). Só os tiles da vista, no zoom da tela
  mais uma prévia 3 níveis acima, num pool próprio, os mais novos e os do
  centro primeiro, descartando os que saíram da tela; LRU de 128 MB na GPU;
  tile que falta mostra o nível mais grosso já carregado; nada bloqueia.
- Desenhado primeiro em tudo (mapa, painéis, cortina com a opção "Basemap
  only", PNG e vista exportados), pela grade de warp da 0.22.0 até um quadro
  em EPSG:3857 em volta da camada ativa. A opacidade esmaece os tiles em
  direção ao fundo (níveis sobrepostos não se misturam duas vezes). Atribuição
  sempre no canto inferior direito, também no PNG e na tag de copyright do
  GeoTIFF.
- Desligado por padrão: com "None" nada toca a rede nem o driver WMS; com uma
  fonte salva, nada antes do 3º quadro. Sem série aberta (ou sem CRS) não há
  mapa de fundo: o espaço do mapa é a grade da camada ativa.
- Medido (vista de 1600×900, 80 tiles): Esri 1,5 s, EOX 2,4 s, OSM 3,8 s da
  internet; ~55 ms do cache do GDAL; primeiro tile em 80–270 ms. Pedir a vista
  custa 7 µs por quadro; render de 400×300 0,20 → 0,25 ms; primeiro quadro
  igual (~150 ms).
- Certificados: no Windows o curl do conda usa Schannel (repositório do
  sistema). No Linux/macOS o curl do conda procura o `cacert.pem` no prefixo
  de build, que não existe no pacote: o Janus aponta para o bundle do sistema
  ou para o `share/ssl/cacert.pem` empacotado (`platform::caBundlePath`).
- Autoteste sem rede: pirâmide `file://` local; pixels conferidos contra o
  PROJ (3508 pixels, diferença 0), prévia no tile que falta, por baixo das
  camadas, opacidade, painéis, cortina, exportação; nenhum dataset WMS aberto
  com "None".

### 0.22.0 — Fase 9: reprojeção e ROI em todas as camadas
- Camadas com outro CRS ou com grade rotacionada passam a ser desenhadas no
  mapa: **grade de warp na GPU** (64 células no lado maior, dobrada até o erro
  bilinear nos centros das células ficar < 0,05 px, no máximo 512), em RG32F
  lida com `texelFetch` e interpolada no shader (GL e Metal) em precisão
  total: a filtragem da GPU tem só ~8 bits nos pesos (~0,4 px de erro numa
  cena). Mesmo CRS sem rotação continua no caminho afim.
- Cursor, pinos, séries das outras camadas, ROI, pedidos de tiles, vista ao
  trocar a camada ativa e transecto usam a transformação **exata** do PROJ
  (OGR), criada uma vez por par de camadas (`src/reproject.*`).
- Tiles de detalhe e resultados do Zeit de camadas reprojetadas também passam
  pelo warp (painéis de mapa e cortina; no mapa principal as camadas não
  ativas ficam no overview, como já era).
- Medido numa cena Landsat (7441×7317, UTM 23S): erro de 0,002 px contra UTM
  22S, 0,013 px contra EPSG:4326, 0,0005 px numa grade rotacionada 10°; grade
  feita em ~1–2 ms; custo por quadro desprezível (0,03 → 0,08 ms num render de
  400×300 com leitura de volta).
- **ROI em cada camada visível** ("All visible layers"): os pixels com centro
  dentro do retângulo (nas reprojetadas, polígono de 64 pontos aplicado como
  máscara por scanline, lido pelo cache em resolução total quando existe),
  com média e p10–p90 no gráfico e uma coluna por camada na tabela.
- Autoteste com F = B em UTM 22S (gdalwarp, vizinho mais próximo, giro de
  ~2,3°): séries idênticas às de B, ROI igual a 4e-5, 7500/7500 pixels do mapa
  em resolução total iguais aos de B; o CI gera e passa F.
- Limites: transformações só na thread principal; antimeridiano e polos podem
  perder células da grade; Copy CSV não inclui as ROIs das outras camadas;
  ROIs de camadas categóricas não vão para o gráfico.

### 0.21.1 — ícone novo
- O ícone do programa (Windows, macOS, Linux e instalador) passa a ser o da
  identidade visual (`branding/icones`: a cabeça dupla em pixels); saiu o
  `tools/make_icon.py`, que desenhava o antigo (camadas + série).

### 0.21.0 — Fase 13: cache em resolução total
- Blocos de 64×64 **por data**, não blocos com todas as datas (como o backlog
  previa): um bloco com o tempo inteiro só existe depois de ler todas as datas
  (cubo de ~9 GB na RAM ou uma segunda passada). Por data, o cache sai da
  mesma leitura do overview, cada data vale assim que é gravada e a retomada é
  data a data, como no cache do overview. Série = 41 leituras de ~11 KB:
  **0,7–1,0 ms**.
- Compressão sem perda: bits do float como inteiro, diferença com o pixel
  anterior, zigzag, planos de bytes, zstd nível 1 pelo registro de compressores
  do GDAL (`CPLGetCompressor`; sem dependência nova; zlib se o GDAL não tiver
  zstd). Medido em blocos reais: 1,42× (zstd puro 1,17×, shuffle + zstd 1,31×,
  deflate 1,43× mas ~10× mais lento, lz4 1,32×); **6,3–6,7 GB por cena** (8,9 GB
  brutos). Valores bit a bit iguais aos do GDAL, inclusive em janelas
  subamostradas (aritmética do vizinho mais próximo do `rasterio.cpp` do GDAL
  3.11; 300 janelas reais, 0 diferenças).
- No HD, na primeira abertura, a passada do overview lê as datas inteiras e
  monta os dois caches: uma thread lê o arquivo em sequência na frente (4 MB
  por vez) e 4 threads decodificam e comprimem a partir do cache do SO. Medido
  a frio: 48,7 s para 38 datas (overview + cache, ~150 MB/s do disco) contra
  52,1 s para 41 datas só do overview. O multi-thread do próprio GDAL (uma
  tarefa por faixa de 1 linha) dava 79 s; só a thread de leitura, 68–71 s.
  Com o overview já em cache, o cache total é montado numa passada própria em
  segundo plano (~54 s por cena).
- Medido: série exata **250–500 ms a frio do HD → 0,7–1,0 ms do cache**; ROI de
  256×256 × 41 datas **~2 s → ~21 ms**. Série, pinos, ROI e tiles de detalhe
  usam o cache nas datas que ele tem e o arquivo nas outras.
- Configuração (painel Performance, guardada no `.ini`): sob demanda / séries
  em HD (padrão) / todas; orçamento de 64 GB com poda pelo uso mais antigo,
  sem apagar caches em uso, e checagem do espaço livre; botão "Build it now" /
  "Resume" por série.
- Limites: as leituras do cache só foram medidas logo depois de montá-lo
  (arquivo ainda na RAM; após reiniciar, estimado 2–4 ms); durante a passada
  própria no HD, pinos e tiles das datas que faltam ainda disputam o disco;
  em COG, janelas subamostradas do cache usam o vizinho mais próximo em
  resolução total (o GDAL usaria o overview do arquivo).

### 0.20.0 — Fase 10: cortina e transecto espaço-tempo
- **Cortina (swipe)**: View → Swipe ou `S`; divisória arrastável no mapa
  principal (arrastá-la nunca move o mapa, nem cria pino ou ROI). À direita,
  outra camada ou a mesma em outra data/modo, escolhida na barra acima do
  mapa: a mesma barra dos painéis de mapa (`uiViewBar`). A comparação é um
  `MapView` desenhado pelo `renderView` num alvo próprio da GPU (slot 1) e
  recortado por coordenadas de textura; com Δ, pede os tiles das duas datas.
- **Transecto (Hovmöller)**: `Ctrl` + arrastar (Mac: `Command`), ou `T` /
  View → Draw transect, traça a linha. Painel **Transect**: distância desde A
  (metros; haversine em EPSG:4xxx) × datas, nas cores da camada (classes nos
  categóricos); valores ou anomalias (cada lugar menos a sua média). Hover
  mostra distância, data e valor e marca o ponto no mapa; clique muda a data;
  Copy CSV. Preenchido na hora pelo overview e depois em resolução total no
  pool interativo (no HD, só depois do overview). A linha segue a camada ativa
  pelas coordenadas geográficas.
- A imagem do transecto é uma textura float desenhada como tile pelo próprio
  renderizador do mapa: nada novo em GL/Metal.
- Autoteste: os dois lados da cortina; o transecto contra o overview e contra
  os valores exatos (16 datas em ~15 ms, erro máximo 5,7e-7); cores da imagem
  e das classes.
- Para depois: transecto na exportação PNG; transecto de várias camadas; linhas
  proporcionais ao tempo real; a barra de status mostrar o valor do lado da
  comparação.

### 0.19.0 — Fase 11: exportação
- **Mapa em PNG** (File → Export map as PNG): renderizado fora da tela a 1×,
  2× ou 4× a resolução da tela (não ampliado), com rótulo de data/modo, barra
  de cores ou legenda de classes, pinos e ROI desenhados na CPU com a fonte do
  ImGui assada no tamanho final. Fundo escuro, branco ou **transparente**: o
  alfa sai de dois renders (sobre preto e sobre branco), sem mudar os shaders.
- **GeoTIFF**: valores da camada ativa na data (o que `readWindow` devolve:
  banda, índice, máscara), área visível ou imagem inteira, Float32, mesma
  grade e CRS, nodata NaN; a **vista renderizada** em RGBA georreferenciado; e
  os **resultados do Zeit** (clique direito no painel Layers → Save as GeoTIFF).
- Tudo com GDAL (MEM + CreateCopy; sem dependência nova), em threads próprias
  com progresso e cancelamento (janela File → Exports); cada arquivo é gravado
  como `.part` e renomeado no fim. `Gpu::readMap` em GL e Metal; diálogo de
  salvar nos três sistemas (`platform::saveFileDialog`).
- Medido (7441×7317 Float32, SSD): valores da imagem inteira em 0,85 s (73 MB);
  PNG 4× (6400×3600) com 80 ms na thread principal + 1,8 s em segundo plano.
- O autoteste exporta de verdade e relê com o GDAL: PNG igual pixel a pixel ao
  mapa, valores iguais à fonte, georreferência conferida.
- Para depois: buscar os tiles do nível da escala exportada (hoje exporta o que
  está carregado), estatísticas e todas as datas em GeoTIFF multibanda, tabela
  de cores para séries categóricas, exportar os painéis de mapa.

### 0.18.0 — Fase 12: mapas de diferença e de maior queda
- Modo **Diferença (valor − referência)**: referência = uma data fixa (padrão:
  a primeira) ou a data anterior (t−1), escolhida no painel Display; paleta
  divergente (BrBG) com faixa automática simétrica em torno de 0; cada camada e
  cada painel de mapa guarda a sua referência.
- Modos **Maior queda: data / magnitude**: a maior queda entre observações
  válidas consecutivas (NaN pulado), datada na observação mais baixa. Sai na
  mesma passada das estatísticas: magnitude em `stats1.w`, data em `stats2`
  (R32F, +22 MB por 5,5 Mpx). A tabela de estatísticas mostra os valores exatos
  calculados na CPU. Desligados para séries categóricas.
- A Δ em resolução total lê os tiles das **duas** datas
  (`forEachVisible(t, t2)`): ficar no overview (4–8× mais grosso) esconderia
  justamente as mudanças pequenas.
- Medido (RTX 3060, 5,5 Mpx × 41): o shader leva ~26 ms; o gargalo era trazer
  as estatísticas para a CPU (zerar o `std::vector` ~30 ms + `glGetTexImage`
  ~45 ms). Com leitura via PBO para um array não inicializado: **~67 ms** no
  total já com as estatísticas novas (antes ~87 ms; ~110 ms sem o PBO).
- Os modos novos são os números 10–12 (os antigos não mudam; uma tabela define
  a ordem no menu). O autoteste de UI compara GPU × CPU em todos os pixels
  (~24 mil quedas, diferença máxima 0) e as cores do mapa.
- Feita por um sub-agente num worktree, em paralelo com as fases 10, 11 e 13.

### Depois da 0.17.0 (sem versão própria)
- `Ctrl+T` abre um painel de mapa; `Ctrl+W` fecha o painel em foco (senão o
  último aberto); o mapa principal nunca fecha.
- Cache do overview gravado data a data, com retomada (ver "Leitura em HD
  mecânico").

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
  (`shown`); o Janus lê as bandas do pixel em segundo plano e escreve um VRT por
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
  data faltante = banda sem dado) e **mínimo de observações por ano**; o Janus
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
- **Ponte** `zeit_bridge/tsv_zeit_bridge.py` (hoje `janus_zeit_bridge.py`) no repositório do tsv (o Zeit não foi
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
