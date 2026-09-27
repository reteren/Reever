//! Нарезка потока на фразы по детектору речи. Владелец: воркер `vad`.

use crate::speech::{samples, VoiceDetector};

#[derive(Clone, Debug)]
pub struct SegmenterConfig {
    /// Сколько аудио ДО начала речи включать в сегмент (не срезать первый слог).
    pub pre_roll_ms: u32,
    /// Минимальная длительность речи, чтобы считать её началом фразы (щелчки — нет).
    pub min_speech_ms: u32,
    /// Тишина, после которой фраза считается законченной. Меняется на лету.
    pub end_silence_ms: u32,
    /// Жёсткий предел длины сегмента — принудительное завершение.
    pub max_segment_ms: u32,
    /// Мягкий предел (0 — выкл.): после него фраза заканчивается уже на короткой
    /// паузе (`SOFT_END_SILENCE_MS`), чтобы длинная диктовка резалась между словами,
    /// а не по жёсткому пределу посреди слова.
    pub soft_max_segment_ms: u32,
}

/// Пауза, завершающая фразу после мягкого предела длины.
const SOFT_END_SILENCE_MS: u32 = 250;

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            pre_roll_ms: 300,
            min_speech_ms: 120,
            end_silence_ms: 600,
            max_segment_ms: 15_000,
            soft_max_segment_ms: 0,
        }
    }
}

#[derive(Debug)]
pub enum SegmentEvent {
    /// Началась речь (после min_speech_ms).
    SpeechStart,
    /// Фраза закончилась; аудио с pre-roll, без хвостовой тишины сверх ~200 мс.
    SpeechEnd(Vec<i16>),
}

pub struct Segmenter {
    cfg: SegmenterConfig,
    frame_remainder: Vec<i16>,
    pre_roll: Vec<i16>,
    candidate_prefix: Vec<i16>,
    candidate_audio: Vec<i16>,
    current_audio: Vec<i16>,
    candidate_speech_samples: usize,
    phrase_samples: usize,
    trailing_silence_samples: usize,
    active: bool,
    pending_end: Option<Vec<i16>>,
    vad_needs_reset: bool,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        Self {
            cfg,
            frame_remainder: Vec::new(),
            pre_roll: Vec::new(),
            candidate_prefix: Vec::new(),
            candidate_audio: Vec::new(),
            current_audio: Vec::new(),
            candidate_speech_samples: 0,
            phrase_samples: 0,
            trailing_silence_samples: 0,
            active: false,
            pending_end: None,
            vad_needs_reset: false,
        }
    }

    pub fn set_end_silence_ms(&mut self, ms: u32) {
        self.cfg.end_silence_ms = ms;
        let limit = samples(ms);
        if self.active
            && self.trailing_silence_samples > 0
            && self.trailing_silence_samples >= limit
        {
            if let Some(segment) = self.finish_segment() {
                self.pending_end = Some(segment);
            }
        }
    }

    /// Пределы длины фразы и pre-roll: у диктовки они длиннее, чем у коротких команд.
    pub fn set_limits(&mut self, pre_roll_ms: u32, soft_max_segment_ms: u32, max_segment_ms: u32) {
        self.cfg.pre_roll_ms = pre_roll_ms;
        self.cfg.soft_max_segment_ms = soft_max_segment_ms;
        self.cfg.max_segment_ms = max_segment_ms;
    }

    /// Кормит произвольный по длине кусок; кадры для `vad` режет сам.
    pub fn push(&mut self, pcm: &[i16], vad: &mut dyn VoiceDetector) -> Vec<SegmentEvent> {
        let mut events = Vec::new();
        if let Some(segment) = self.pending_end.take() {
            events.push(SegmentEvent::SpeechEnd(segment));
        }

        let frame_len = vad.frame_len();
        if frame_len == 0 {
            return events;
        }

        // Остаток переносится между вызовами, поэтому размер входных чанков не
        // влияет на границы кадров или на итоговую фразу.
        let mut buffered = std::mem::take(&mut self.frame_remainder);
        buffered.extend_from_slice(pcm);
        let complete_len = buffered.len() / frame_len * frame_len;

        for start in (0..complete_len).step_by(frame_len) {
            if self.vad_needs_reset {
                vad.reset();
                self.vad_needs_reset = false;
            }
            let frame = &buffered[start..start + frame_len];
            let speech = vad.is_speech(frame);
            self.process_frame(frame, speech, &mut events);
        }

        self.frame_remainder
            .extend_from_slice(&buffered[complete_len..]);
        zero_vec(&mut buffered);
        events
    }

    fn process_frame(&mut self, frame: &[i16], speech: bool, events: &mut Vec<SegmentEvent>) {
        if self.active {
            self.current_audio.extend_from_slice(frame);
            self.phrase_samples = self.phrase_samples.saturating_add(frame.len());
            if speech {
                self.trailing_silence_samples = 0;
            } else {
                self.trailing_silence_samples =
                    self.trailing_silence_samples.saturating_add(frame.len());
            }

            let max_len = samples(self.cfg.max_segment_ms).max(1);
            let soft_max = self.cfg.soft_max_segment_ms;
            let silence_limit = if soft_max > 0 && self.phrase_samples >= samples(soft_max) {
                samples(self.cfg.end_silence_ms.min(SOFT_END_SILENCE_MS))
            } else {
                samples(self.cfg.end_silence_ms)
            };
            if self.phrase_samples >= max_len
                || (self.trailing_silence_samples > 0
                    && self.trailing_silence_samples >= silence_limit)
            {
                if let Some(segment) = self.finish_segment() {
                    events.push(SegmentEvent::SpeechEnd(segment));
                }
            }
            return;
        }

        if speech {
            if self.candidate_audio.is_empty() {
                self.candidate_prefix.clone_from(&self.pre_roll);
            }
            self.candidate_audio.extend_from_slice(frame);
            self.candidate_speech_samples =
                self.candidate_speech_samples.saturating_add(frame.len());

            if self.candidate_speech_samples >= samples(self.cfg.min_speech_ms) {
                let candidate_len = self.candidate_audio.len();
                self.current_audio = std::mem::take(&mut self.candidate_prefix);
                self.current_audio.extend_from_slice(&self.candidate_audio);
                zero_vec(&mut self.candidate_audio);
                zero_vec(&mut self.pre_roll);
                self.candidate_speech_samples = 0;
                // Ограничение длины фразы не должно съедать лимит за pre-roll.
                self.phrase_samples = candidate_len;
                self.active = true;
                events.push(SegmentEvent::SpeechStart);

                if self.phrase_samples >= samples(self.cfg.max_segment_ms).max(1) {
                    if let Some(segment) = self.finish_segment() {
                        events.push(SegmentEvent::SpeechEnd(segment));
                    }
                }
            }
        } else if self.candidate_audio.is_empty() {
            append_bounded(&mut self.pre_roll, frame, samples(self.cfg.pre_roll_ms));
        } else {
            // Короткий импульс не становится речью. Его звук остаётся доступен
            // в pre-roll для возможного следующего слога, но кандидат сбрасывается.
            append_bounded(
                &mut self.pre_roll,
                &self.candidate_audio,
                samples(self.cfg.pre_roll_ms),
            );
            append_bounded(&mut self.pre_roll, frame, samples(self.cfg.pre_roll_ms));
            zero_vec(&mut self.candidate_audio);
            zero_vec(&mut self.candidate_prefix);
            self.candidate_speech_samples = 0;
        }
    }

    fn finish_segment(&mut self) -> Option<Vec<i16>> {
        if !self.active {
            return None;
        }

        let keep_silence = self.trailing_silence_samples.min(samples(200));
        let speech_end = self
            .current_audio
            .len()
            .saturating_sub(self.trailing_silence_samples);
        let output_len = speech_end.saturating_add(keep_silence);
        self.current_audio[output_len..].fill(0);
        self.current_audio.truncate(output_len);

        // Отдаём копию потребителю и затираем собственную рабочую копию до
        // очистки; внутренний буфер сохраняет ёмкость, но не звук.
        let completed = self.current_audio.clone();
        self.scrub_working_buffers();
        self.active = false;
        self.phrase_samples = 0;
        self.trailing_silence_samples = 0;
        self.vad_needs_reset = true;
        Some(completed)
    }

    fn scrub_working_buffers(&mut self) {
        zero_vec(&mut self.frame_remainder);
        zero_vec(&mut self.pre_roll);
        zero_vec(&mut self.candidate_prefix);
        zero_vec(&mut self.candidate_audio);
        zero_vec(&mut self.current_audio);
        self.candidate_speech_samples = 0;
    }

    /// Идёт ли сейчас фраза.
    pub fn in_speech(&self) -> bool {
        self.active
    }

    /// Аудио текущей незаконченной фразы (для промежуточного распознавания).
    pub fn current_audio(&self) -> &[i16] {
        if self.active {
            &self.current_audio
        } else {
            &[]
        }
    }

    /// Сброс с затиранием всех буферов нулями (приватность: голос не остаётся в RAM).
    pub fn reset(&mut self) {
        self.scrub_working_buffers();
        if let Some(mut pending) = self.pending_end.take() {
            pending.fill(0);
        }
        self.phrase_samples = 0;
        self.trailing_silence_samples = 0;
        self.active = false;
        self.vad_needs_reset = false;
    }
}

fn append_bounded(dst: &mut Vec<i16>, samples: &[i16], limit: usize) {
    if limit == 0 {
        return;
    }
    dst.extend_from_slice(samples);
    if dst.len() > limit {
        let excess = dst.len() - limit;
        dst[..excess].fill(0);
        dst.drain(..excess);
    }
}

fn zero_vec(buf: &mut Vec<i16>) {
    buf.fill(0);
    buf.clear();
}

#[cfg(test)]
mod tests {
    use super::{SegmentEvent, Segmenter, SegmenterConfig};
    use crate::speech::VoiceDetector;

    struct MarkerVad {
        frame_len: usize,
        calls: usize,
        resets: usize,
    }

    impl MarkerVad {
        fn new(frame_len: usize) -> Self {
            Self {
                frame_len,
                calls: 0,
                resets: 0,
            }
        }
    }

    impl VoiceDetector for MarkerVad {
        fn frame_len(&self) -> usize {
            self.frame_len
        }

        fn is_speech(&mut self, frame: &[i16]) -> bool {
            self.calls += 1;
            frame.iter().any(|sample| *sample >= 500)
        }

        fn reset(&mut self) {
            self.resets += 1;
        }
    }

    fn config(pre_roll_ms: u32, min_speech_ms: u32, end_silence_ms: u32) -> SegmenterConfig {
        SegmenterConfig {
            pre_roll_ms,
            min_speech_ms,
            end_silence_ms,
            max_segment_ms: 5_000,
            soft_max_segment_ms: 0,
        }
    }

    fn append_frames(dst: &mut Vec<i16>, marker: i16, frame_len: usize, count: usize) {
        for _ in 0..count {
            dst.extend(std::iter::repeat(marker).take(frame_len));
        }
    }

    fn summarize(events: Vec<SegmentEvent>) -> Vec<(u8, Vec<i16>)> {
        events
            .into_iter()
            .map(|event| match event {
                SegmentEvent::SpeechStart => (0, Vec::new()),
                SegmentEvent::SpeechEnd(audio) => (1, audio),
            })
            .collect()
    }

    #[test]
    fn pause_of_300ms_does_not_split_phrase_with_600ms_endpoint() {
        let mut segmenter = Segmenter::new(config(0, 20, 600));
        let mut vad = MarkerVad::new(160);
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 1_000, 160, 20); // 200 мс речи
        append_frames(&mut pcm, 0, 160, 30); // пауза 300 мс
        append_frames(&mut pcm, 1_000, 160, 20);
        append_frames(&mut pcm, 0, 160, 60); // конец после 600 мс

        let events = summarize(segmenter.push(&pcm, &mut vad));
        assert_eq!(events.iter().filter(|(kind, _)| *kind == 0).count(), 1);
        let ended: Vec<_> = events.iter().filter(|(kind, _)| *kind == 1).collect();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1.len(), 90 * 160);
    }

    #[test]
    fn pause_of_700ms_splits_phrases() {
        let mut segmenter = Segmenter::new(config(0, 20, 600));
        let mut vad = MarkerVad::new(160);
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 1_000, 160, 20);
        append_frames(&mut pcm, 0, 160, 70);
        append_frames(&mut pcm, 1_000, 160, 20);
        append_frames(&mut pcm, 0, 160, 70);

        let events = summarize(segmenter.push(&pcm, &mut vad));
        assert_eq!(events.iter().filter(|(kind, _)| *kind == 0).count(), 2);
        assert_eq!(events.iter().filter(|(kind, _)| *kind == 1).count(), 2);
    }

    #[test]
    fn pre_roll_contains_audio_before_the_first_speech_frame() {
        let mut cfg = config(100, 20, 100);
        cfg.max_segment_ms = 1_000;
        let mut segmenter = Segmenter::new(cfg);
        let mut vad = MarkerVad::new(160);
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 10, 160, 15); // в ring-буфере останутся последние 100 мс
        append_frames(&mut pcm, 1_000, 160, 10);

        let events = segmenter.push(&pcm, &mut vad);
        assert!(events
            .iter()
            .any(|event| matches!(event, SegmentEvent::SpeechStart)));
        assert!(segmenter.in_speech());
        assert_eq!(
            segmenter.current_audio().len(),
            100 * 16_000 / 1_000 + 10 * 160
        );
        assert!(segmenter.current_audio()[..1_600]
            .iter()
            .all(|sample| *sample == 10));
    }

    #[test]
    fn max_segment_forces_completion() {
        let mut cfg = config(0, 10, 1_000);
        cfg.max_segment_ms = 200;
        let mut segmenter = Segmenter::new(cfg);
        let mut vad = MarkerVad::new(160);
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 1_000, 160, 20);

        let events = summarize(segmenter.push(&pcm, &mut vad));
        let ended: Vec<_> = events.iter().filter(|(kind, _)| *kind == 1).collect();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1.len(), 200 * 16_000 / 1_000);
        assert!(!segmenter.in_speech());
        segmenter.push(&[0; 160], &mut vad);
        assert_eq!(vad.resets, 1);
    }

    #[test]
    fn arbitrary_input_chunk_sizes_match_single_chunk_result() {
        let frame_len = 160;
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 10, frame_len, 25);
        append_frames(&mut pcm, 1_000, frame_len, 20);
        append_frames(&mut pcm, 0, frame_len, 30);
        append_frames(&mut pcm, 1_000, frame_len, 15);
        append_frames(&mut pcm, 0, frame_len, 60);

        let mut whole = Segmenter::new(config(100, 20, 600));
        let mut whole_vad = MarkerVad::new(frame_len);
        let expected = summarize(whole.push(&pcm, &mut whole_vad));

        let mut chunked = Segmenter::new(config(100, 20, 600));
        let mut chunked_vad = MarkerVad::new(frame_len);
        let mut actual = Vec::new();
        let mut offset = 0;
        let sizes = [37, 503, 1_234, 71, 2_003, 319];
        let mut size_index = 0;
        while offset < pcm.len() {
            let end = (offset + sizes[size_index % sizes.len()]).min(pcm.len());
            actual.extend(summarize(chunked.push(&pcm[offset..end], &mut chunked_vad)));
            offset = end;
            size_index += 1;
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn min_speech_discards_a_50ms_click() {
        let mut segmenter = Segmenter::new(config(100, 120, 600));
        let mut vad = MarkerVad::new(160);
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 1_000, 160, 5);
        append_frames(&mut pcm, 0, 160, 70);

        let events = segmenter.push(&pcm, &mut vad);
        assert!(events.is_empty());
        assert!(!segmenter.in_speech());
    }

    #[test]
    fn lowering_end_silence_finishes_an_active_phrase_immediately() {
        let mut segmenter = Segmenter::new(config(0, 20, 900));
        let mut vad = MarkerVad::new(160);
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 1_000, 160, 20);
        append_frames(&mut pcm, 0, 160, 30);
        let events = segmenter.push(&pcm, &mut vad);
        assert!(events
            .iter()
            .any(|event| matches!(event, SegmentEvent::SpeechStart)));
        assert!(segmenter.in_speech());

        segmenter.set_end_silence_ms(200);
        assert!(!segmenter.in_speech());
        let ended = summarize(segmenter.push(&[], &mut vad));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].0, 1);
    }

    #[test]
    fn reset_scrubs_active_audio_and_pending_output() {
        let mut segmenter = Segmenter::new(config(0, 20, 900));
        let mut vad = MarkerVad::new(160);
        let mut pcm = Vec::new();
        append_frames(&mut pcm, 1_000, 160, 20);
        append_frames(&mut pcm, 0, 160, 30);
        segmenter.push(&pcm, &mut vad);
        assert!(segmenter.in_speech());

        segmenter.set_end_silence_ms(200);
        assert!(segmenter.pending_end.is_some());
        segmenter.reset();
        assert!(!segmenter.in_speech());
        assert!(segmenter.pending_end.is_none());
        assert!(segmenter.current_audio.is_empty());
        assert!(segmenter.pre_roll.is_empty());
        assert!(segmenter.candidate_audio.is_empty());
    }
}
