"""Janus' wavelet scalogram (crates/janus-app/src/wavelet.rs, Analysis > Cycles) against Torrence & Compo's
algorithm as their own Python code (waveletFunctions.py: wavelet, wave_signif) computes it, in numpy and scipy.

    JANUS_WAVELET_DUMP=out.json cargo test -p janus-app dump_for_the_reference -- --ignored
    python tools/wavelet_reference.py out.json

The series is put on the same regular grid (numpy's linear interpolation, held past the ends), its mean and linear
trend taken out, its lag-1 autocorrelation estimated as Janus does (that choice is not theirs to make); the rest is
theirs: the padding, the Morlet daughter, the power over the sample variance, the cone of influence, red noise's
95 % level, the global spectrum's with the degrees of freedom of eq. 23 (n points averaged, as the paper; their
example subtracts the scale from n). Prints the largest differences and exits 1 past the tolerances.
"""
import json
import sys

import numpy as np
from scipy.stats import chi2

DAY = 86400.0


def morlet(k, scale, dt, k0=6.0):
    n = len(k)
    expnt = -((scale * k - k0) ** 2) / 2.0 * (k > 0.0)
    norm = np.sqrt(scale * k[1]) * (np.pi ** (-0.25)) * np.sqrt(n)
    return norm * np.exp(expnt) * (k > 0.0)


def wavelet(y, dt, dj, s0, J):
    n1 = len(y)
    x = y - np.mean(y)
    base2 = int(np.fix(np.log(n1) / np.log(2) + 0.4999))
    x = np.concatenate((x, np.zeros(2 ** (base2 + 1) - n1)))
    n = len(x)
    k = np.arange(1, int(n / 2) + 1) * (2.0 * np.pi) / (n * dt)
    k = np.concatenate(([0.0], k, -k[int((n - 1) / 2) - 1 :: -1]))
    f = np.fft.fft(x)
    scale = s0 * 2.0 ** (np.arange(0, J + 1) * dj)
    wave = np.array([np.fft.ifft(f * morlet(k, s, dt)) for s in scale])[:, :n1]
    k0 = 6.0
    ff = 4.0 * np.pi / (k0 + np.sqrt(2.0 + k0**2))
    period = ff * scale
    # their Matlab's ranges (the Python port's give n1 + 1 points when n1 is odd)
    rise = np.arange(1, int(np.floor((n1 + 1) / 2 - 1)) + 1)
    fall = np.arange(1, int(np.floor(n1 / 2 - 1)) + 1)[::-1]
    coi = ff / np.sqrt(2.0) * dt * np.concatenate(([1e-5], rise, fall, [1e-5]))
    return wave, period, scale, coi


def main():
    d = json.load(open(sys.argv[1]))
    t = np.array(d["times"])
    v = np.array([np.nan if x is None else x for x in d["values"]])
    dt, n = d["dt"], d["n"]
    ok = np.isfinite(v)
    grid = t[0] + np.arange(n) * dt
    x = np.interp(grid, t[ok], v[ok])
    x = x - x.mean()
    i = np.arange(n)
    x = x - np.polyval(np.polyfit(i, x, 1), i)
    variance = np.std(x, ddof=1) ** 2
    alpha = min(max(np.sum(x[:-1] * x[1:]) / np.sum(x[:-1] ** 2), 0.0), 0.95)
    dj, s0 = 0.125, 2.0 * dt
    J = len(d["periods"]) - 1
    wave, period, scale, coi = wavelet(x, dt, dj, s0, J)
    power = np.abs(wave) ** 2 / variance
    freq = dt / period
    theor = (1.0 - alpha**2) / (1.0 - 2.0 * alpha * np.cos(freq * 2.0 * np.pi) + alpha**2)
    signif = theor * chi2.ppf(0.95, 2) / 2.0
    glob = power.mean(axis=1)
    dof = np.maximum(2.0 * np.sqrt(1.0 + (n * dt / 2.32 / scale) ** 2), 2.0)
    gsig = theor * chi2.ppf(0.95, dof) / dof

    ours = np.array(d["power"]).reshape(len(d["periods"]), n)
    checks = [
        ("periods (days)", np.array(d["periods"]), period / DAY, 1e-9),
        ("power", ours, power, 1e-6),
        ("variance", np.array([d["variance"]]), np.array([variance]), 1e-9),
        ("alpha", np.array([d["alpha"]]), np.array([alpha]), 1e-9),
        ("95 % level", np.array(d["signif"]), signif, 1e-9),
        ("cone (days)", np.array(d["coi"]), coi / DAY, 1e-9),
        ("global spectrum", np.array(d["global"]), glob, 1e-6),
        ("global 95 % level", np.array(d["global_signif"]), gsig, 1e-9),
    ]
    bad = False
    for name, a, b, tol in checks:
        rel = np.max(np.abs(a - b) / np.maximum(np.abs(b), 1e-9 * np.max(np.abs(b))))
        print(f"{name:20s} largest relative difference {rel:.2e} (tolerance {tol:.0e})")
        bad |= not rel <= tol
    print(f"{n} steps of {dt / DAY:.2f} days, {len(period)} scales, alpha {alpha:.3f}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
