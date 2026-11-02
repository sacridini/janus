//! What a trained model's validation means, in plain words (the Classification window's Model tab): how good the
//! accuracy is and how it was measured, the classes it misses and what they are taken for, a network that learned
//! its samples by heart, an accuracy measured on too few samples, and what to do about each. From the summary the
//! model file keeps (`training`), so a model opened later is read the same way.

use serde_json::Value;

/// One line of advice: how much it matters (0 for information, 1 to look at, 2 to act on) and what it says.
#[derive(Clone, Debug, PartialEq)]
pub struct Advice {
    pub level: u8,
    pub text: String,
}

/// A class found less often than this (its producer's accuracy) is named.
const WEAK_CLASS: f64 = 0.7;

fn pct(v: f64) -> String {
    format!("{:.0} %", v * 100.0)
}

/// The advice on a model's `training` summary, its classes' names in order.
pub fn advise(training: &Value, classes: &[String]) -> Vec<Advice> {
    let mut out = Vec::new();
    let mut add = |level: u8, text: String| out.push(Advice { level, text });
    let v = &training["validation"];
    let Some(oa) = v["overall_accuracy"].as_f64().filter(|x| x.is_finite()) else {
        return out;
    };
    let n = training["samples"]["validation"].as_u64().unwrap_or(0);
    let split = training["samples"]["split"].as_str().unwrap_or("samples");
    let spatial = matches!(split, "blocks" | "knndm" | "clusters" | "buffer");
    // how good, and how it was measured
    let how = if spatial {
        "measured on places as far from the training as the map is"
    } else if split == "patches" {
        "measured on patches at random, some maybe next to the training ones: the map far from them may do worse"
    } else if split == "places" {
        "measured on places at random, some maybe next to the training ones: the map far from your samples may do \
         worse"
    } else {
        "measured on pixels of the same places the model learned from: too good to trust"
    };
    let (level, verdict) = if oa >= 0.85 {
        (0, "Good")
    } else if oa >= 0.7 {
        (1, "Fair")
    } else {
        (2, "Weak")
    };
    add(level, format!("{verdict}: about {} of the samples get the right class ({how}).", pct(oa)));
    if n > 0 && n < 100 {
        let half = 1.96 * (oa * (1.0 - oa) / n as f64).sqrt();
        add(
            1,
            format!(
                "Measured on {n} samples only: the accuracy could well be anywhere from {} to {}. More samples, \
                 from more places, measure it better.",
                pct((oa - half).max(0.0)),
                pct((oa + half).min(1.0))
            ),
        );
    }
    // the classes it misses, and what they are taken for
    let confusion: Vec<Vec<u64>> = v["confusion"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| r.as_array().map(|x| x.iter().map(|c| c.as_u64().unwrap_or(0)).collect()).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    for (i, row) in confusion.iter().enumerate() {
        let total: u64 = row.iter().sum();
        let Some(name) = classes.get(i) else { continue };
        if total == 0 {
            continue;
        }
        let found = row.get(i).copied().unwrap_or(0) as f64 / total as f64;
        if found >= WEAK_CLASS {
            continue;
        }
        let other = (0..row.len()).filter(|&j| j != i).max_by_key(|&j| row[j]).filter(|&j| row[j] > 0);
        add(
            if found < 0.5 { 2 } else { 1 },
            match other.and_then(|j| classes.get(j)) {
                Some(o) => format!(
                    "{name}: only {} of its samples found, most often taken for {o}. Add samples of both, from more \
                     places, where they differ most; or join them if the map need not tell them apart.",
                    pct(found)
                ),
                None => {
                    format!("{name}: only {} of its samples found. Add samples of it, from more places.", pct(found))
                }
            },
        );
    }
    // learned by heart: the network's accuracy on its own samples far above the others'; the forest's out of bag
    if let Some(e) = training["best_epoch"]
        .as_u64()
        .and_then(|b| training["history"].as_array().and_then(|h| h.get(b.saturating_sub(1) as usize)))
    {
        let own = e["train_acc"].as_f64().unwrap_or(f64::NAN);
        if own - oa >= 0.15 {
            add(
                1,
                format!(
                    "The network learned its samples by heart: {} right on them, {} on the others. More places of \
                     each class help more than more epochs.",
                    pct(own),
                    pct(oa)
                ),
            );
        }
    }
    if let Some(oob) = training["oob"]["overall_accuracy"].as_f64().filter(|x| x.is_finite())
        && spatial
        && oob - oa >= 0.1
    {
        add(
            1,
            format!(
                "Out of bag the forest scores {}, far from the training {}: neighbouring samples look alike, and the \
                 lower number is the one to expect on the map. Samples spread over the area close the gap.",
                pct(oob),
                pct(oa)
            ),
        );
    }
    if split == "places" {
        add(
            1,
            "Validation by kNNDM (Validation, in the Method tab) would test the model as far from the samples as the \
             map is."
                .into(),
        );
    }
    add(
        0,
        "This accuracy chooses and improves the model. The map's own accuracy and its areas come from a sample drawn \
         over the map at random (Validation & uncertainty)."
            .into(),
    );
    out.sort_by_key(|a| std::cmp::Reverse(a.level));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn classes() -> Vec<String> {
        ["Forest", "Pasture", "Soy"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn good_and_spatial() {
        let t = json!({
            "samples": { "validation": 600, "split": "knndm" },
            "validation": { "overall_accuracy": 0.91, "confusion": [[200, 5, 0], [10, 180, 10], [0, 20, 175]] },
            "best_epoch": 2,
            "history": [{ "train_acc": 0.9 }, { "train_acc": 0.95 }],
        });
        let a = advise(&t, &classes());
        assert_eq!(a[0].level, 0, "{a:?}");
        assert!(
            a.iter().any(|x| x.text.starts_with("Good: about 91 %") && x.text.contains("as far from the training"))
        );
        assert!(a.iter().all(|x| x.level == 0), "{a:?}");
    }

    #[test]
    fn weak_class_overfit_few_samples() {
        let t = json!({
            "samples": { "validation": 60, "split": "places" },
            "validation": { "overall_accuracy": 0.68, "confusion": [[20, 0, 0], [2, 8, 10], [0, 7, 13]] },
            "best_epoch": 1,
            "history": [{ "train_acc": 0.99 }],
        });
        let a = advise(&t, &classes());
        let text: Vec<&str> = a.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(a[0].level, 2, "{text:?}");
        assert!(text.iter().any(|t| t.starts_with("Weak: about 68 %") && t.contains("at random")));
        assert!(text.iter().any(|t| t.starts_with("Pasture: only 40 %") && t.contains("taken for Soy")));
        assert!(text.iter().any(|t| t.starts_with("Soy: only 65 %") && t.contains("taken for Pasture")));
        assert!(text.iter().any(|t| t.starts_with("The network learned its samples by heart: 99 %")));
        assert!(text.iter().any(|t| t.starts_with("Measured on 60 samples only")));
        assert!(text.iter().any(|t| t.starts_with("Validation by kNNDM")));
        assert!(a.windows(2).all(|p| p[0].level >= p[1].level));
    }

    #[test]
    fn forest_out_of_bag_gap() {
        let t = json!({
            "samples": { "validation": 400, "split": "blocks" },
            "validation": { "overall_accuracy": 0.78, "confusion": [[150, 50], [38, 162]] },
            "oob": { "overall_accuracy": 0.97 },
        });
        let a = advise(&t, &classes()[..2]);
        assert!(a.iter().any(|x| x.text.starts_with("Out of bag the forest scores 97 %")), "{a:?}");
        assert!(a.iter().any(|x| x.text.starts_with("Fair: about 78 %")));
        // no model: nothing to say
        assert!(advise(&json!({}), &classes()).is_empty());
        // a dense network's patches: held out at random, kNNDM not offered (its training has no folds)
        let t = json!({
            "samples": { "validation": 400, "split": "patches" },
            "validation": { "overall_accuracy": 0.9, "confusion": [[180, 20], [20, 180]] },
        });
        let a = advise(&t, &classes()[..2]);
        assert!(a.iter().any(|x| x.text.contains("patches at random")), "{a:?}");
        assert!(!a.iter().any(|x| x.text.contains("kNNDM")), "{a:?}");
    }
}
