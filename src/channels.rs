//! Per-channel constraints used by drafting and by `validate_content`.

#[derive(Debug, Clone, Copy)]
pub struct ChannelSpec {
    pub name: &'static str,
    pub max_chars: usize,
    pub max_hashtags: usize,
    pub requires_media: bool,
    /// Accepted image aspect ratios as (min, max) width/height.
    pub aspect_min: f64,
    pub aspect_max: f64,
    pub max_images: usize,
}

const SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: "instagram", max_chars: 2200, max_hashtags: 30, requires_media: true, aspect_min: 0.8, aspect_max: 1.91, max_images: 10 },
    ChannelSpec { name: "facebook", max_chars: 63206, max_hashtags: 30, requires_media: false, aspect_min: 0.5, aspect_max: 2.0, max_images: 10 },
    ChannelSpec { name: "x", max_chars: 280, max_hashtags: 5, requires_media: false, aspect_min: 0.5, aspect_max: 3.0, max_images: 4 },
    ChannelSpec { name: "linkedin", max_chars: 3000, max_hashtags: 10, requires_media: false, aspect_min: 0.5, aspect_max: 2.4, max_images: 9 },
    ChannelSpec { name: "tiktok", max_chars: 2200, max_hashtags: 20, requires_media: true, aspect_min: 0.5, aspect_max: 1.0, max_images: 35 },
    ChannelSpec { name: "threads", max_chars: 500, max_hashtags: 5, requires_media: false, aspect_min: 0.5, aspect_max: 2.0, max_images: 10 },
    ChannelSpec { name: "pinterest", max_chars: 500, max_hashtags: 20, requires_media: true, aspect_min: 0.5, aspect_max: 1.0, max_images: 1 },
    ChannelSpec { name: "bluesky", max_chars: 300, max_hashtags: 5, requires_media: false, aspect_min: 0.5, aspect_max: 3.0, max_images: 4 },
    ChannelSpec { name: "youtube", max_chars: 5000, max_hashtags: 15, requires_media: true, aspect_min: 1.0, aspect_max: 2.0, max_images: 1 },
];

/// `twitter` is accepted as an alias of `x`.
pub fn canonical(channel: &str) -> String {
    match channel.trim().to_ascii_lowercase().as_str() {
        "twitter" => "x".into(),
        c => c.into(),
    }
}

pub fn spec(channel: &str) -> Option<&'static ChannelSpec> {
    let c = canonical(channel);
    SPECS.iter().find(|s| s.name == c)
}

/// Aspect ratio as "w:h" reduced to common forms.
pub fn aspect_label(w: u32, h: u32) -> String {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    if w == 0 || h == 0 {
        return "0:0".into();
    }
    let g = gcd(w, h);
    format!("{}:{}", w / g, h / g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_lookup() {
        assert_eq!(canonical("Twitter"), "x");
        assert_eq!(spec("twitter").unwrap().max_chars, 280);
        assert!(spec("myspace").is_none());
    }

    #[test]
    fn aspect_labels() {
        assert_eq!(aspect_label(1080, 1080), "1:1");
        assert_eq!(aspect_label(1920, 1080), "16:9");
        assert_eq!(aspect_label(0, 10), "0:0");
    }
}
