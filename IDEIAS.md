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
- Modo desenvolvedor: apontar para um Python próprio com o Zeit editável.

## Roadmap

| Fase | Versão | Conteúdo | Status |
|---|---|---|---|
| 0 | 0.3.0 | Migração para `tsv`, tudo em inglês, README, IDEIAS.md, abertura medida e otimizada | concluída |
| 1 | 0.4.0 | Runtime Python embutido, servidor do Zeit, conexão assíncrona, menu de ferramentas, painel de tarefas, **LandTrendr** (pixel + cena) | próxima |
| 2 | 0.5.0 | **Mann-Kendall** (variantes do Zeit) e **BFAST / BFAST Lite / BFAST Monitor** | planejada |
| 3 | 0.6.0 | Cubo com várias bandas por data + máscara de qualidade; **fenologia** e **CCDC** | planejada |

## Ideias (backlog)

### Análise
- Mapa de **quebra**: ano e magnitude da maior queda por pixel (barato na GPU).
- Mapa de **tendência significativa** (p-valor do Mann-Kendall por pixel; esconder o que é ruído).
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

### Produto
- "Atualizar Zeit" dentro do app (baixa a wheel nova para o runtime privado).
- Ferramentas de IA do Zeit como download opcional (PyTorch é pesado).
- Releases no GitHub com o instalador anexado.
- Linux (os stubs de `platform.cpp` existem; falta testar).
- Escala de interface (DPI) e fonte TTF para telas 4K.

## Histórico

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
