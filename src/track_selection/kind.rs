//! Track kind, descriptor, and preference types.

/// The media kind carried by a selected DASH adaptation set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// An audio adaptation set.
    Audio,
    /// A video adaptation set.
    Video,
    /// A subtitle or caption adaptation set (`text/vtt`, TTML, or in-band fMP4 text tracks).
    Text,
    /// A trick-play video adaptation set (`http://dashif.org/guidelines/trickmode`).
    TrickPlay,
    /// A thumbnail image adaptation set (`image/jpeg`, `image/png`, `image/bmp`, or other
    /// `image/*` MIME types; often with `thumbnail_tile`).
    Image,
}

/// A DASH descriptor used for track metadata and accessibility matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDescriptor {
    /// Descriptor scheme URI, such as `urn:mpeg:dash:role:2011`.
    pub scheme_id_uri: String,
    /// Optional descriptor value. When absent, any value under the scheme matches.
    pub value: Option<String>,
}

impl TrackDescriptor {
    /// Create a preference that matches any descriptor value under `scheme_id_uri`.
    pub fn scheme(scheme_id_uri: impl Into<String>) -> Self {
        Self {
            scheme_id_uri: scheme_id_uri.into(),
            value: None,
        }
    }

    /// Create a preference that matches both a descriptor scheme and value.
    pub fn new(scheme_id_uri: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            scheme_id_uri: scheme_id_uri.into(),
            value: Some(value.into()),
        }
    }
}

/// Ordered preferences and output limit for one media kind.
///
/// Preference lists are fallback lists: earlier entries rank ahead of later entries, and
/// adaptation sets that match none rank last. An empty list does not affect ranking.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackPreference {
    /// Preferred RFC 5646 language ranges, in priority order.
    pub languages: Vec<String>,
    /// Preferred DASH `Role@value` values, in priority order.
    pub roles: Vec<String>,
    /// Preferred RFC 6381 codec names or prefixes, in priority order.
    pub codecs: Vec<String>,
    /// Preferred DASH accessibility descriptors, in priority order.
    pub accessibility: Vec<TrackDescriptor>,
    /// Maximum number of tracks of this kind. `None` retains every compatible track.
    pub max_tracks: Option<usize>,
}

impl TrackPreference {
    /// Add a preferred RFC 5646 language range.
    pub fn language(mut self, language: impl Into<String>) -> Self {
        self.languages.push(language.into());
        self
    }

    /// Add a preferred DASH role value.
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Add a preferred RFC 6381 codec name or prefix.
    pub fn codec(mut self, codec: impl Into<String>) -> Self {
        self.codecs.push(codec.into());
        self
    }

    /// Add a preferred accessibility descriptor.
    pub fn accessibility(mut self, descriptor: TrackDescriptor) -> Self {
        self.accessibility.push(descriptor);
        self
    }

    /// Limit how many tracks of this media kind are selected.
    pub fn max_tracks(mut self, max_tracks: usize) -> Self {
        self.max_tracks = Some(max_tracks);
        self
    }
}

/// User preferences for selecting audio, video, text, trick-play, and image adaptation sets.
///
/// The default retains all audio and video tracks and **no** text, trick-play, or image tracks.
/// Use [`TrackPreference::max_tracks`] on [`Self::text`], [`Self::trick_play`], or [`Self::image`]
/// to enable auxiliary delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSelection {
    /// Audio-track preferences. Set `max_tracks` above one to retain multiple preferred languages
    /// or roles.
    pub audio: TrackPreference,
    /// Video-track preferences.
    pub video: TrackPreference,
    /// Subtitle and caption preferences. Disabled by default (`max_tracks(0)`).
    pub text: TrackPreference,
    /// Trick-play video preferences. Disabled by default (`max_tracks(0)`).
    pub trick_play: TrackPreference,
    /// Thumbnail image preferences. Disabled by default (`max_tracks(0)`).
    pub image: TrackPreference,
}

impl Default for TrackSelection {
    fn default() -> Self {
        Self {
            audio: TrackPreference::default(),
            video: TrackPreference::default(),
            text: TrackPreference::default().max_tracks(0),
            trick_play: TrackPreference::default().max_tracks(0),
            image: TrackPreference::default().max_tracks(0),
        }
    }
}

impl TrackSelection {
    /// Replace the audio-track preferences.
    pub fn with_audio(mut self, audio: TrackPreference) -> Self {
        self.audio = audio;
        self
    }

    /// Replace the video-track preferences.
    pub fn with_video(mut self, video: TrackPreference) -> Self {
        self.video = video;
        self
    }

    /// Replace subtitle and caption preferences.
    pub fn with_text(mut self, text: TrackPreference) -> Self {
        self.text = text;
        self
    }

    /// Replace trick-play video preferences.
    pub fn with_trick_play(mut self, trick_play: TrackPreference) -> Self {
        self.trick_play = trick_play;
        self
    }

    /// Replace thumbnail image preferences.
    pub fn with_image(mut self, image: TrackPreference) -> Self {
        self.image = image;
        self
    }
}
