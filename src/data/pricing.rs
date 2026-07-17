//! Тарифы из pricing.json. Модель подбирается по подстроке (длинное совпадение
//! выигрывает), кэш считается из input через множители, fast — премиум Claude.

use serde_json::Value;

struct Rate {
    input: f64,
    output: f64,
}

struct Model {
    m: String,
    input: f64,
    output: f64,
    fast: Option<Rate>,
}

pub struct Pricing {
    models: Vec<Model>,
    read: f64,
    write_5m: f64,
    write_1h: f64,
    def: Rate,
}

impl Pricing {
    pub fn load(text: &str) -> Pricing {
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let cm = &v["cache_multipliers"];
        let f = |x: &Value, k: &str, d: f64| x.get(k).and_then(|n| n.as_f64()).unwrap_or(d);

        let mut models = Vec::new();
        if let Some(arr) = v["models"].as_array() {
            for m in arr {
                let fast = m.get("fast").map(|fv| Rate {
                    input: f(fv, "input", 0.0),
                    output: f(fv, "output", 0.0),
                });
                models.push(Model {
                    m: m.get("match")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    input: f(m, "input", 0.0),
                    output: f(m, "output", 0.0),
                    fast,
                });
            }
        }
        // длинное совпадение выигрывает
        models.sort_by(|a, b| b.m.len().cmp(&a.m.len()));

        Pricing {
            models,
            read: f(cm, "read", 0.1),
            write_5m: f(cm, "write_5m", 1.25),
            write_1h: f(cm, "write_1h", 2.0),
            def: Rate {
                input: f(&v["default"], "input", 3.0),
                output: f(&v["default"], "output", 15.0),
            },
        }
    }

    fn rate(&self, model: &str, speed: &str) -> (f64, f64) {
        let model = model.to_lowercase();
        for m in &self.models {
            if !m.m.is_empty() && model.contains(&m.m) {
                if speed == "fast" {
                    if let Some(fr) = &m.fast {
                        return (fr.input, fr.output);
                    }
                }
                return (m.input, m.output);
            }
        }
        (self.def.input, self.def.output)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cost(
        &self,
        model: &str,
        speed: &str,
        inp: u64,
        cread: u64,
        cw5: u64,
        cw1h: u64,
        out: u64,
    ) -> f64 {
        let (ir, or) = self.rate(model, speed);
        (inp as f64 * ir
            + cread as f64 * ir * self.read
            + cw5 as f64 * ir * self.write_5m
            + cw1h as f64 * ir * self.write_1h
            + out as f64 * or)
            / 1_000_000.0
    }
}
