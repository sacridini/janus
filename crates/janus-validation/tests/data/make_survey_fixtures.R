# Fixtures for tests/survey_parity.rs (and Zeit's tests/test_accuracy_survey.py, from the same script): the
# area of each class and of each transition between two dates, by the direct, difference and ratio estimators of a
# stratified random sample, as R's survey package (Lumley) computes them: svytotal, svyratio and svymean on a
# svydesign with the strata and their weights (the area of a stratum over its points), without a finite population
# correction, as zeit.accuracy and Janus.
#
# A made-up landscape of 120 x 100 pixels of 30 m (0.09 ha): Forest (1), Pasture (2) and Crop (3) at a first and a
# second date, a few patches of forest cleared and of pasture cropped between them; two maps of the dates with
# errors; strata that are not the maps' classes (an older alert map: alerts, a buffer around them, the rest), and a
# stratified random sample of 80 + 60 + 50 points. Also the same sample assessing the second date's classes, and a
# sample stratified by the second map's classes (where the three estimators agree).
#
#   Rscript crates/janus-validation/tests/data/make_survey_fixtures.R crates/janus-validation/tests/data/accuracy_survey.json
library(survey)
library(jsonlite)

set.seed(20141114)
H <- 120
W <- 100
PIXEL <- 0.09  # ha
names3 <- c("Forest", "Pasture", "Crop")
row <- matrix(rep(0:(H - 1), times = W), H, W)
col <- matrix(rep(0:(W - 1), each = H), H, W)

# the truth at the two dates
ref_from <- ifelse(col < 60, 1L, ifelse(row < 70, 2L, 3L))
ref_from[row %/% 10 == 3 & col %/% 10 == 7] <- 1L          # a forest patch in the pasture
ref_to <- ref_from
ref_to[row >= 10 & row < 22 & col >= 20 & col < 32] <- 2L   # cleared
ref_to[row >= 80 & row < 88 & col >= 5 & col < 15] <- 2L    # cleared
ref_to[row >= 40 & row < 50 & col >= 62 & col < 75] <- 3L   # pasture cropped
ref_to[row >= 100 & row < 104 & col >= 85 & col < 95] <- 1L # crop abandoned, regrowth

flip <- function(a, p) {
    k <- runif(length(a)) < p
    a[k] <- ((a[k] - 1L + sample(1:2, sum(k), replace = TRUE)) %% 3L) + 1L
    a
}
map_from <- flip(ref_from, 0.06)
changed <- ref_from != ref_to
map_to <- map_from
seen <- changed & runif(length(changed)) < 0.8                      # 80% of the changes mapped
map_to[seen] <- ref_to[seen]
map_to[!changed] <- flip(map_from[!changed], 0.015)                  # commission of change
map_to <- matrix(map_to, H, W)

# strata from another, older map: its alerts, a buffer of 2 pixels around them, the rest
alert <- (changed & runif(length(changed)) < 0.7) | runif(length(changed)) < 0.02
alert <- matrix(alert, H, W)
near <- matrix(FALSE, H, W)
for (dy in -2:2) for (dx in -2:2) {
    r <- (1:H) + dy
    c <- (1:W) + dx
    ok_r <- r >= 1 & r <= H
    ok_c <- c >= 1 & c <= W
    near[(1:H)[ok_r], (1:W)[ok_c]] <- near[(1:H)[ok_r], (1:W)[ok_c]] | alert[r[ok_r], c[ok_c]]
}
strata <- ifelse(alert, 2L, ifelse(near, 3L, 1L))

draw <- function(strata, n) {
    pts <- integer(0)
    for (h in sort(unique(as.vector(strata)))) {
        cells <- which(strata == h)
        pts <- c(pts, cells[sample.int(length(cells), n[[as.character(h)]])])
    }
    pts
}

estimates <- function(strata, map, ref, classes, cells) {
    area_h <- tapply(rep(PIXEL, length(strata)), as.vector(strata), sum)
    d <- data.frame(h = as.vector(strata)[cells], m = map[cells], r = ref[cells])
    n_h <- table(d$h)
    d$w <- as.numeric(area_h[as.character(d$h)] / n_h[as.character(d$h)])
    des <- svydesign(ids = ~1, strata = ~h, weights = ~w, data = d)
    out <- list()
    for (k in classes) {
        d$y <- as.numeric(d$r == k)
        d$x <- as.numeric(d$m == k)
        d$yx <- d$y * d$x
        d$dd <- d$y - d$x
        des <- update(des, y = d$y, x = d$x, yx = d$yx, dd = d$dd)
        mapped <- sum(map == k) * PIXEL
        direct <- svytotal(~y, des)
        diff <- svytotal(~dd, des)
        res <- list(mapped = mapped,
                    direct = c(coef(direct)[[1]], SE(direct)[[1]]),
                    difference = c(mapped + coef(diff)[[1]], SE(diff)[[1]]))
        if (sum(d$x) > 0) {
            rt <- svyratio(~y, ~x, des)
            res$ratio <- c(coef(rt)[[1]] * mapped, SE(rt)[[1]] * mapped)
            u <- svyratio(~yx, ~x, des)
            res$users <- c(coef(u)[[1]], SE(u)[[1]])
        } else {
            res$ratio <- c(NA, NA)
            res$users <- c(NA, NA)
        }
        if (sum(d$y) > 0) {
            p <- svyratio(~yx, ~y, des)
            res$producers <- c(coef(p)[[1]], SE(p)[[1]])
        } else {
            res$producers <- c(NA, NA)
        }
        out[[as.character(k)]] <- res
    }
    des <- update(des, agree = as.numeric(d$r == d$m))
    o <- svymean(~agree, des)
    list(classes = out, overall = c(coef(o)[[1]], SE(o)[[1]]))
}

name_of <- function(k) {
    if (k > 10) paste(names3[k %/% 10], "->", names3[k %% 10]) else names3[k]
}
named <- function(e) {
    names(e$classes) <- vapply(as.integer(names(e$classes)), name_of, "")
    e
}

cells <- draw(strata, list("1" = 80, "2" = 60, "3" = 50))
pair <- function(a, b) 10L * a + b
codes_tr <- sort(unique(c(as.vector(pair(map_from, map_to)), pair(ref_from, ref_to)[cells])))
transitions <- named(estimates(strata, pair(map_from, map_to), pair(ref_from, ref_to), codes_tr, cells))
classes <- named(estimates(strata, map_to, ref_to, 1:3, cells))
cells2 <- draw(map_to, list("1" = 70, "2" = 60, "3" = 50))
by_map <- named(estimates(map_to, map_to, ref_to, 1:3, cells2))

flat <- function(a) as.vector(t(a))  # row-major
pts <- function(cells, strata) {
    data.frame(row = row[cells], col = col[cells], stratum = strata[cells],
               map_from = map_from[cells], map_to = map_to[cells], ref_from = ref_from[cells], ref_to = ref_to[cells])
}
out <- list(
    survey = as.character(packageVersion("survey")),
    shape = c(H, W), pixel_area = PIXEL, names = names3, z = qnorm(0.975),
    strata = flat(strata), map_from = flat(map_from), map_to = flat(map_to),
    points = pts(cells, strata), points_by_map = pts(cells2, map_to),
    transitions = transitions, classes = classes, by_map = by_map
)
path <- commandArgs(TRUE)[1]
if (is.na(path)) path <- "tests/data/accuracy_survey.json"
write_json(out, path, digits = NA, auto_unbox = TRUE, dataframe = "columns",
           na = "null")
