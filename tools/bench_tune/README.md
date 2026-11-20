# Benchmark: Tune on samples against the same loop in Python

Janus's *Tune on samples* (`janus-zeit/src/tune.rs`) runs a tool's raster kernel once per set of parameters on all the
reference points (a one-row chunk) and scores the dates. This benchmark runs the same search with the Zeit Python
package (`zeit-cdts`, the loop of the script *Copy Python* writes in the Tune window), on the same synthetic series and
the same sets, and compares the scores and the time.

## What runs

- **Data** (written by the Rust side): P = 300 points of annual series 1990-2021 (LandTrendr: half stable, half with a
  persistent drop of 0.12, 0.2 or 0.4 at a year of 1998-2014, noise 0.04) and of monthly series 2005-2016 with a season
  (BFAST: a drop of 0.3 in 2007-2008, noise 0.05); N = 200 sets of the tool's default parameters (a Latin hypercube
  over them, plus the defaults and the current values: 202, minus any repeated one).
- **Janus**: `tune::run` with `threads = 4` (the sets in parallel, each on one thread), from a test of the app's own
  binary (`tune_ui::tests::bench_tune_against_python`, ignored): the app's allocator (mimalloc, which zeit-cdts'
  extension uses too) matters, because the pixel kernels allocate a lot (see the note under the table).
- **Python**: for each set, `zeit.landtrendr` + `zeit.extract_events`, or `zeit.bfast`, on the `(time, 1, points)` cube
  of the points, the calendar year of the change taken from the result and scored with the functions of the generated
  script (`<tool>_functions.py`, written by `janus_zeit::python::tune_functions`: the very code of `tune.py`). Two ways
  to use 4 threads: `threads` (the generated script's: the sets one after the other, Zeit's `n_jobs=4` over the points
  of each) and `procs` (4 processes, a share of the sets each, `n_jobs=1`).
- **Compared**: the six scores of every set (omission, commission, F1, dated F1, dates within the tolerance, mean date
  error): the largest absolute difference, and whether the same ones are NaN. And the seconds of the loop (the
  reads and the imports are left out of both).

## How to run it

Release build (the engine's speed is what is measured). From the repository's root, with at most 8 cores in all (the
benchmark itself uses 4):

```powershell
$env:JANUS_BENCH_DIR = "C:\path\to\a\scratch\folder"   # must exist
$env:JANUS_BENCH_THREADS = "4"                         # optional; 300 points and 200 sets by default
cargo test --release -j 8 -p janus-app --bin janus bench_tune_against_python -- --ignored --nocapture
conda run --no-capture-output -n zeit python tools/bench_tune/bench_tune.py $env:JANUS_BENCH_DIR --mode threads --workers 4
conda run --no-capture-output -n zeit python tools/bench_tune/bench_tune.py $env:JANUS_BENCH_DIR --mode procs --workers 4
```

`OMP_NUM_THREADS`, `RAYON_NUM_THREADS` and the like should be 4 or less, and the process pinned to at most 8 cores.

## Result

Measured on 2026-11-18 (Windows 11, release build of the app's binary, 4 threads on each side, 8 cores at most
for everything running, Zeit 3.8.0 from its source tree, zeit-core pinned by Janus at `b98da66` = 3.6.0):

| Tool | Points | Sets | Janus (4 threads) | Python `threads` (`n_jobs=4`) | Python `procs` (4 x `n_jobs=1`) | Largest score difference |
|------|-------:|-----:|------------------:|------------------------------:|--------------------------------:|-------------------------:|
| LandTrendr | 300 | 202 | 0.16 s | 1.23 s (7.6x) | 4.55 s (28x) | 0 (every score of every set equal) |
| BFAST | 300 | 202 | 11.81 s | 12.69 s (1.1x) | 14.66 s (1.2x) | 0 (every score of every set equal) |

(202 sets = the 200 of the Latin hypercube, the defaults and the current values. The times are the loop only; the
first Python call, the imports and the reads are outside both. One run each; the Python's times varied by a few
percent from run to run.)

- **Results.** The six scores of all 202 sets of both tools are equal to the last bit (difference 0, the same NaNs),
  which shows that the generated script's year extraction (`yod` of `zeit.extract_events`; the break of the largest
  magnitude of `zeit.bfast`) and its scoring are the engine's.
- **Time.** LandTrendr: the engine runs the kernel on one row of pixels with no Python around it (about 3 ms of
  kernel a set); the Python loop pays `zeit.landtrendr`'s and `extract_events`' xarray and dask work (a Dataset, a dask
  graph) on every set, which is most plausibly most of its 6 to 23 ms a set (not profiled); the engine is 7 to 28 times
  faster. BFAST: the kernel dominates
  (about 2 ms a point and set) and both sides call the same zeit-core, so they are within 10% of each other.
- **A thing the benchmark found.** The first version of the Rust side ran in `janus-zeit`'s own test binary, with the
  system allocator: BFAST took 33 s there (the same with `--profile dist`: it is not the optimization level), 2.8 times
  the 11.8 s of the app's binary. The pixel kernels allocate many small vectors, and the system heap probably serializes four
  threads doing it (not profiled); the app (`main.rs`) and zeit-cdts' extension both use mimalloc. The search in the app runs with
  mimalloc; a program that links `janus-zeit` and runs the search on several threads should set it too.
