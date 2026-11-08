use serde::Deserialize;

use crate::Usage;

/// Prices in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Pricing {
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    /// Price of prompt tokens served from the provider's cache; defaults to the input price.
    #[serde(default)]
    pub cached_input_usd_per_mtok: Option<f64>,
}

impl Pricing {
    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        let cached = usage.cached_tokens.min(usage.prompt_tokens);
        let uncached = usage.prompt_tokens - cached;
        let cached_price = self
            .cached_input_usd_per_mtok
            .unwrap_or(self.input_usd_per_mtok);
        (uncached as f64 * self.input_usd_per_mtok
            + cached as f64 * cached_price
            + usage.completion_tokens as f64 * self.output_usd_per_mtok)
            / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_prompt_tokens_use_the_cached_price() {
        let pricing = Pricing {
            input_usd_per_mtok: 2.0,
            output_usd_per_mtok: 10.0,
            cached_input_usd_per_mtok: Some(0.5),
        };
        let usage = Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 100_000,
            cached_tokens: 400_000,
        };
        // 600k × $2 + 400k × $0.5 + 100k × $10, per million.
        assert!((pricing.cost_usd(&usage) - 2.4).abs() < 1e-9);

        let no_cache_price = Pricing {
            cached_input_usd_per_mtok: None,
            ..pricing
        };
        assert!((no_cache_price.cost_usd(&usage) - 3.0).abs() < 1e-9);
    }
}
