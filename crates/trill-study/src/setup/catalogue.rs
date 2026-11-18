//! What the setup fetches for studies: each part with where it is
//! published, how big it is, what it must hash to, and its licence in the
//! words of its source.
//!
//! Trill ships none of this and hosts none of it. Every file is fetched by
//! the user from where its makers (or whoever converted it) publish it,
//! pinned to one version by its checksum: a file that is replaced where it
//! is published is refused, not run.
//!
//! The pins are the versions that were measured together on a machine:
//! change one, and the timed test and the comparisons against known
//! recordings are to be run again.

use crate::models::real::{
    ANY_CARD, BEATS, NVIDIA, RUNTIME, STEMS, STEMS_BEST, VOICE, WORD_TIMES, WORDS, WORDS_PROCESSOR,
};
use trill_fetch::{Fetched, Keep, Part};

const WHISPER: &str =
    "https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a";
const WHISPER_LICENCE: &str = "OpenAI: \"Whisper's code and model weights are released under the MIT License.\" Converted to ONNX by the onnx-community project.";
const DEMUCS: &str = "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a";
const DEMUCS_LICENCE: &str = "Unclear. Meta's Demucs: the code is under the MIT license; of the weights its author writes \"The license of the weights is not clear: MusDB contains some data that were only usable for research purpose.\" Converted to ONNX by HRSadeghi/demucs-onnx.";
const ALIGNER: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-2025-11-12/resolve/98ab94b2a45a0d13f852c80ae77c52766ac1ea25";

fn part(id: &str, name: &str, what: &str, licence: &str, from: &str, needs: &[&str], files: Vec<Fetched>) -> Part {
    Part {
        id: id.to_owned(),
        name: name.to_owned(),
        what: what.to_owned(),
        licence: licence.to_owned(),
        credit: String::new(),
        from: from.to_owned(),
        needs: needs.iter().map(|id| (*id).to_owned()).collect(),
        files,
    }
}

/// A file of a repository, kept by its own name.
fn of(repository: &str, path: &str, sha256: &str, size: u64) -> Fetched {
    Fetched::file(&format!("{repository}/{path}"), sha256, size, path.rsplit('/').next().unwrap_or(path))
}

fn members(pairs: &[(&str, &str)]) -> Keep {
    Keep::Members(pairs.iter().map(|(from, to)| ((*from).to_owned(), (*to).to_owned())).collect())
}

/// What the transcriber reads beside its two networks, the same for a card and for the processor.
fn whisper_words() -> Vec<Fetched> {
    vec![
        of(WHISPER, "vocab.json", "e2aa043ef015641d363d8288e7c241c85e36a5c761fb303598e0710233344387", 1_036_558),
        of(WHISPER, "added_tokens.json", "3c51f66c4c21f9e126970078f11ae77a78c74aee8df606ee9daba86e467108e0", 34_648),
        of(
            WHISPER,
            "generation_config.json",
            "16f95291d2f47c944d3c2b19390bba7965666555c1ea2a0bdc850d1fab45612f",
            3_897,
        ),
    ]
}

/// The parts there are for this system, in the order they are shown.
/// Elsewhere than on Windows the runtime and the parts that drive a
/// graphics card are not pinned yet: the models' own files are the same.
pub fn parts() -> Vec<Part> {
    let mut parts = Vec::new();
    if cfg!(all(windows, target_arch = "x86_64")) {
        parts.push(part(
            RUNTIME,
            "The models' runtime",
            "ONNX Runtime 1.32.0, which runs every model: on the processor by itself, on a graphics card with one of the two parts below. Nothing can be studied without it.",
            "Microsoft: MIT License.",
            "github.com/microsoft/onnxruntime (its release)",
            &[],
            vec![Fetched::archive(
                "https://github.com/microsoft/onnxruntime/releases/download/v1.32.0/onnxruntime-win-x64-1.32.0.zip",
                "aaa1d04fcf86bd31e84b90bc0cbde80e632bd0720dcc5d8ba05c4fe45c09d127",
                96_945_131,
                members(&[
                    ("onnxruntime-win-x64-1.32.0/lib/onnxruntime.dll", "onnxruntime.dll"),
                    ("onnxruntime-win-x64-1.32.0/lib/onnxruntime_providers_shared.dll", "onnxruntime_providers_shared.dll"),
                ]),
            )],
        ));
        parts.push(part(
            ANY_CARD,
            "For a graphics card of any make",
            "Runs the models on a graphics card through the system's own graphics interface, with nothing of the card's maker: version 0.5.0, the one that was measured working (the one after it fails on NVIDIA cards).",
            "Microsoft: MIT License; its notices list what it is built on.",
            "pypi.org/project/onnxruntime-ep-webgpu (Microsoft's package)",
            &[RUNTIME],
            vec![Fetched::archive(
                "https://files.pythonhosted.org/packages/7e/36/9935a5b7851099da3da81d9e04f3021bcc61a5a1039e6c098cf08e5c6607/onnxruntime_ep_webgpu-0.5.0-py3-none-win_amd64.whl",
                "713b94e7b4ce5979a356e19b926ecba2c0d4ba24f857ef4bbb11c75c37cc5b61",
                13_608_886,
                members(&[
                    ("onnxruntime_ep_webgpu/onnxruntime_providers_webgpu.dll", "onnxruntime_providers_webgpu.dll"),
                    ("onnxruntime_ep_webgpu/dxcompiler.dll", "dxcompiler.dll"),
                ]),
            )],
        ));
        parts.push(part(
            NVIDIA,
            "For an NVIDIA card, through NVIDIA's own libraries",
            "About twice as fast on an NVIDIA card as the part for any card, and a gigabyte to fetch. Optional: the part for any card runs NVIDIA cards too.",
            "The provider is Microsoft's, MIT License. NVIDIA's libraries (cuDNN, cuBLAS) are not free software: they come from NVIDIA's own packages under the \"LICENSE AGREEMENT FOR NVIDIA SOFTWARE DEVELOPMENT KITS\", which you accept by fetching them. Trill ships and hosts none of it.",
            "github.com/microsoft/onnxruntime (the provider) and pypi.org, NVIDIA's packages nvidia-cudnn-cu13 and nvidia-cublas",
            &[RUNTIME],
            vec![
                Fetched::archive(
                    "https://github.com/microsoft/onnxruntime/releases/download/plugin-ep-cuda/v0.4.0/cuda_ep_cuda13_0.4.0_win-x64.zip",
                    "d49409c04f271cbc3a5171edea8da2344794c16e17b31731d7eac222c4a5646a",
                    153_590_304,
                    members(&[("onnxruntime_providers_cuda.dll", "onnxruntime_providers_cuda.dll")]),
                ),
                Fetched::archive(
                    "https://files.pythonhosted.org/packages/48/9c/26e6bdd31c97c389aa565b49c916be0dec17dd61dad6cd33fee286d1b946/nvidia_cudnn_cu13-9.28.1.4-py3-none-win_amd64.whl",
                    "73d874c6bd8a1c75ca040228f77c4e163efe9bab3b65bd85914e2b7334ac68ee",
                    407_065_099,
                    Keep::Under { folder: "nvidia/cudnn/bin/".to_owned(), ending: ".dll".to_owned() },
                ),
                Fetched::archive(
                    "https://files.pythonhosted.org/packages/12/39/e1b1b4adb8b171894d9a89035df0596d448cf73dc403802d24dbe30cc0be/nvidia_cublas-13.10.0.20-py3-none-win_amd64.whl",
                    "c310b749eea855b88fafb906de08d6819b87216c6711640771764fea5dcd8546",
                    427_284_863,
                    members(&[
                        ("nvidia/cu13/bin/x86_64/cublas64_13.dll", "cublas64_13.dll"),
                        ("nvidia/cu13/bin/x86_64/cublasLt64_13.dll", "cublasLt64_13.dll"),
                    ]),
                ),
            ],
        ));
    }
    parts.push(part(
        STEMS,
        "Stems",
        "The model that splits a recording into vocals, drums, bass and the rest (Hybrid Transformer Demucs). Everything a study finds, it finds in the stems.",
        DEMUCS_LICENCE,
        "huggingface.co/HRSadeghi/demucs-onnx",
        &[RUNTIME],
        vec![of(DEMUCS, "htdemucs.onnx", "abaac48bc8fd55fa672268c8f13228b9b00f52f0c4cd10945048982eca4643bc", 168_683_222)],
    ));
    parts.push(part(
        STEMS_BEST,
        "Stems, at their best",
        "Four models, one tuned for each stem: the cleanest stems, at four times the work. Optional.",
        DEMUCS_LICENCE,
        "huggingface.co/HRSadeghi/demucs-onnx",
        &[RUNTIME],
        vec![
            of(
                DEMUCS,
                "htdemucs_ft_0.onnx",
                "4738167ecd97779b0b23e21c754f54d65d7b1e1d077a856ba88295b54a35ba4e",
                168_683_222,
            ),
            of(
                DEMUCS,
                "htdemucs_ft_1.onnx",
                "5f62c38f565e9a73fe60b1917220119fa7b7fc2c259d059a836a9751f0c01072",
                168_683_222,
            ),
            of(
                DEMUCS,
                "htdemucs_ft_2.onnx",
                "f3a57a391d1a26cb1009761d664d44420abe22b7842b7f3147d45a1cdde85a7a",
                168_683_222,
            ),
            of(
                DEMUCS,
                "htdemucs_ft_3.onnx",
                "31baa94de72168b84feba38dbae38c99ac6c3877c4184e5892c837a4ea76f4e5",
                168_683_222,
            ),
        ],
    ));
    parts.push(part(
        BEATS,
        "Beats",
        "The model that hears where the beats and the bar lines are (Beat This). Without it a study does not run: the pulse of the onsets is what one sound is listened to by.",
        "JKU Linz: \"The code and the published model weights are released under the MIT license. Note that some of the training files are fully copyrighted or under limited Creative Commons licenses, and it is up to the user to assess whether this may impact their use case.\" Converted to ONNX by danigb/beat-this-rs.",
        "github.com/danigb/beat-this-rs (its release)",
        &[RUNTIME],
        vec![Fetched::file(
            "https://github.com/danigb/beat-this-rs/releases/download/model-large/beat_this.onnx",
            "5f810debe53459b559127fb55bbad40035bb47cc567b20e501670f968c770f02",
            83_162_650,
            "beat_this.onnx",
        )],
    ));
    parts.push(part(
        VOICE,
        "Where the voice is",
        "The model that hears where someone sings (Silero VAD), so that words are written down only there and none are made of what is not a voice. Wanted whenever the words are; a megabyte.",
        "Silero: \"MIT License\". The file is the one the faster-whisper project (SYSTRAN, MIT License) packages.",
        "pypi.org/project/faster-whisper (SYSTRAN's package)",
        &[RUNTIME],
        vec![Fetched::archive(
            "https://files.pythonhosted.org/packages/05/99/49ee85903dee060d9f08297b4a342e5e0bcfca2f027a07b4ee0a38ab13f9/faster_whisper-1.2.1-py3-none-any.whl",
            "79a66ad50688c0b794dd501dc340a736992a6342f7f95e5811be60b5224a26a7",
            1_118_909,
            members(&[("faster_whisper/assets/silero_vad_v6.onnx", "silero_vad_v6.onnx")]),
        )],
    ));
    let mut on_a_card = vec![
        of(
            WHISPER,
            "onnx/encoder_model_fp16.onnx",
            "fdadc70836e6b028fd5e580417c312208dad073d2d01e509e2d127c1373399d8",
            1_274_342_603,
        ),
        of(
            WHISPER,
            "onnx/decoder_model_fp16.onnx",
            "56026785e9d0ae5c93e76c5d5ac9560e0568051ff664fb56c3a9eb32745bd6c5",
            344_003_183,
        ),
    ];
    on_a_card.extend(whisper_words());
    parts.push(part(
        WORDS,
        "Words, on a graphics card",
        "The model that writes down what is sung (Whisper large-v3-turbo), as a graphics card takes it. Wanted when the models run on a card.",
        WHISPER_LICENCE,
        "huggingface.co/onnx-community/whisper-large-v3-turbo",
        &[RUNTIME],
        on_a_card,
    ));
    let mut on_the_processor = vec![
        of(
            WHISPER,
            "onnx/encoder_model_int8.onnx",
            "e44c0d5cfcc6ad283011602a738fa28dfa1ad7f7540c9503205479072a9cc1ef",
            644_822_094,
        ),
        of(
            WHISPER,
            "onnx/decoder_model_int8.onnx",
            "b54c67a80cf31304d75b8d525393e8758c23f848027d4fa916f26077e9f4bde2",
            439_684_851,
        ),
    ];
    on_the_processor.extend(whisper_words());
    parts.push(part(
        WORDS_PROCESSOR,
        "Words, on the processor",
        "The same model in whole numbers, as a processor takes it. Wanted when the models run on the processor.",
        WHISPER_LICENCE,
        "huggingface.co/onnx-community/whisper-large-v3-turbo",
        &[RUNTIME],
        on_the_processor,
    ));
    parts.push(part(
        WORD_TIMES,
        "Word times",
        "The model that places each word where it is sung (Omnilingual ASR, CTC, 300M). Without it the words have only the times of the phrases they are in.",
        "Meta: \"Omnilingual ASR code and models are released under the Apache 2.0.\" Converted to ONNX by the sherpa-onnx project.",
        "huggingface.co/csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-2025-11-12",
        &[RUNTIME],
        vec![
            of(ALIGNER, "model.onnx", "25f6faecf3f541626e6d3b793103d9544c2f52b0f4f40373b4e2720ac3ff77d5", 1_302_628_362),
            of(ALIGNER, "tokens.txt", "a7a044c52cb29cbe8b0dc1953e92cefd4ca16b0ed968177b6beab21f9a7d0b31", 86_423),
        ],
    ));
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_is_pinned_and_says_whose_it_is() {
        let parts = parts();
        let ids: Vec<&str> = parts.iter().map(|part| part.id.as_str()).collect();
        for wanted in [STEMS, STEMS_BEST, BEATS, VOICE, WORDS, WORDS_PROCESSOR, WORD_TIMES] {
            assert!(ids.contains(&wanted), "{wanted} is not offered");
        }
        for part in &parts {
            assert!(!part.files.is_empty() && part.size() > 0, "{}", part.id);
            assert!(part.licence.len() > 20 && part.from.contains('.') && part.what.ends_with('.'), "{}", part.id);
            for file in &part.files {
                assert!(file.url.starts_with("https://"), "{}", file.url);
                assert!(
                    file.sha256.len() == 64 && file.sha256.chars().all(|letter| letter.is_ascii_hexdigit()),
                    "{}",
                    file.url
                );
                // Pinned to a version: a release, a commit, or a file named by its hash.
                assert!(
                    file.url.contains("/releases/download/")
                        || file.url.contains("/resolve/") && !file.url.contains("/resolve/main/")
                        || file.url.contains("files.pythonhosted.org/packages/"),
                    "{} floats",
                    file.url
                );
            }
            for needed in &part.needs {
                assert!(
                    cfg!(not(windows)) || ids.contains(&needed.as_str()),
                    "{} needs {needed}, which is not offered",
                    part.id
                );
            }
        }
        // The stem models say that their licence is unclear, in their maker's words; the six-stem one is not offered.
        for part in parts.iter().filter(|part| part.id.starts_with("stems")) {
            assert!(
                part.licence.starts_with("Unclear.")
                    && part.licence.contains("The license of the weights is not clear")
            );
            assert!(part.files.iter().all(|file| !file.url.contains("6s")));
        }
        // And the engine takes them as they are: every rule it has for a list holds.
        let engine = trill_fetch::Fetches::new(std::sync::Arc::new(trill_fetch::Nothing));
        engine.add(crate::setup::list("somewhere".into())).unwrap();
    }
}
