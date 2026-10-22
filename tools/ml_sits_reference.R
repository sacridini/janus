# The reference of Janus's TempCNN and L-TAE (crates/janus-ml): sits' own samples split into training and test
# (stratified, 80/20, a fixed seed), sits_tempcnn and sits_lighttae trained with their defaults on the training
# samples (several seeds) and their accuracy on the test samples; written with the samples as JSON that
# `cargo test -p janus-ml --release --test sits -- --ignored` trains Janus on, comparing the accuracies.
#
#     Rscript tools/ml_sits_reference.R [OUT_DIR]
#
# OUT_DIR defaults to crates/janus-ml/tests/data. Needs sits (>= 1.5), torch and luz. On a GPU whose cuDNN torch
# cannot load, run it on the CPU: CUDA_VISIBLE_DEVICES=-1.

suppressMessages(library(sits))
args <- commandArgs(trailingOnly = TRUE)
out_dir <- if (length(args) > 0) args[[1]] else file.path("crates", "janus-ml", "tests", "data")
dir.create(out_dir, showWarnings = FALSE, recursive = TRUE)
seeds <- c(1L, 2L, 3L)

reference <- function(name) {
    s <- get(name, envir = asNamespace("sits"))
    set.seed(42)
    split <- rep("train", nrow(s))
    for (l in unique(s$label)) {
        idx <- which(s$label == l)
        split[sample(idx, round(0.2 * length(idx)))] <- "test"
    }
    train <- s[split == "train", ]
    test <- s[split == "test", ]
    bands <- sits_bands(s)
    accuracy <- function(model) {
        pred <- sits_classify(test, model, progress = FALSE)
        got <- vapply(pred$predicted, function(p) as.character(p$class[[1]]), "")
        mean(got == test$label)
    }
    runs <- list()
    for (m in c("tempcnn", "lighttae")) {
        acc <- c()
        for (seed in seeds) {
            t0 <- Sys.time()
            model <- if (m == "tempcnn") sits_train(train, sits_tempcnn(seed = seed)) else sits_train(train, sits_lighttae(seed = seed))
            a <- accuracy(model)
            cat(sprintf("%s %s seed %d: test accuracy %.4f (%.0f s)\n", name, m, seed, a, as.numeric(Sys.time() - t0, units = "secs")))
            acc <- c(acc, a)
        }
        runs[[m]] <- acc
    }
    samples <- lapply(seq_len(nrow(s)), function(i) {
        ts <- s$time_series[[i]]
        list(label = s$label[[i]], split = split[[i]], values = unname(as.matrix(ts[, bands])))
    })
    out <- list(
        name = name, sits = as.character(packageVersion("sits")), bands = bands,
        timeline = as.character(sits_timeline(s)), labels = sort(unique(s$label)),
        seeds = seeds, accuracy = runs, samples = samples
    )
    jsonlite::write_json(out, file.path(out_dir, paste0("sits_", name, ".json")), digits = NA, auto_unbox = TRUE)
}

for (name in c("samples_modis_ndvi", "samples_l8_rondonia_2bands")) reference(name)
