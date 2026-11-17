//! What the setup fetches: each part with where it is published, how big
//! it is, what it must hash to, and its licence in the words of its source.
//!
//! Trill ships none of this and hosts none of it. Every file is fetched by
//! the user from where its makers (or whoever converted it) publish it,
//! pinned to one version by its checksum: a file that is replaced where it
//! is published is refused, not run.
//!
//! The pins are the versions that were measured together on a machine:
//! change one, and the timed test and the comparisons against known
//! recordings are to be run again.

use crate::models::real::{ANY_CARD, BEATS, NVIDIA, RUNTIME, STEMS, STEMS_BEST, WORD_TIMES, WORDS, WORDS_PROCESSOR};

/// What becomes of a fetched file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Keep {
    /// It is the part's file, under this name.
    As(&'static str),
    /// It is an archive: these of its members are kept, each under the
    /// name after it.
    Members(&'static [(&'static str, &'static str)]),
    /// It is an archive: every member under this folder whose name ends
    /// in this is kept, by its own name.
    Under(&'static str, &'static str),
}

/// One file the setup fetches.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub url: &'static str,
    /// Its SHA-256, in hexadecimal.
    pub sha256: &'static str,
    /// Its size in bytes.
    pub size: u64,
    pub keep: Keep,
}

/// One part of the setup.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The name of its folder among the parts.
    pub id: &'static str,
    pub name: &'static str,
    /// What it is for, and when it is wanted.
    pub what: &'static str,
    /// Its licence, in the words of its source where it has any.
    pub licence: &'static str,
    /// Who publishes the files, and where.
    pub from: &'static str,
    /// The parts it is no use without.
    pub needs: &'static [&'static str],
    pub files: &'static [Fetched],
}

impl Part {
    /// What is fetched for it, in bytes.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

const WHISPER: &str =
    "https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a";
const WHISPER_LICENCE: &str = "OpenAI: \"Whisper's code and model weights are released under the MIT License.\" Converted to ONNX by the onnx-community project.";
const DEMUCS_LICENCE: &str = "Unclear. Meta's Demucs: the code is under the MIT license; of the weights its author writes \"The license of the weights is not clear: MusDB contains some data that were only usable for research purpose.\" Converted to ONNX by HRSadeghi/demucs-onnx.";

macro_rules! whisper_words {
    () => {
        [
            Fetched {
                url: concat!("https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a", "/vocab.json"),
                sha256: "e2aa043ef015641d363d8288e7c241c85e36a5c761fb303598e0710233344387",
                size: 1_036_558,
                keep: Keep::As("vocab.json"),
            },
            Fetched {
                url: concat!("https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a", "/added_tokens.json"),
                sha256: "3c51f66c4c21f9e126970078f11ae77a78c74aee8df606ee9daba86e467108e0",
                size: 34_648,
                keep: Keep::As("added_tokens.json"),
            },
            Fetched {
                url: concat!("https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a", "/generation_config.json"),
                sha256: "16f95291d2f47c944d3c2b19390bba7965666555c1ea2a0bdc850d1fab45612f",
                size: 3_897,
                keep: Keep::As("generation_config.json"),
            },
        ]
    };
}

const WORDS_FILES: [Fetched; 3] = whisper_words!();

/// The parts there are for this system, in the order they are shown.
/// Elsewhere than on Windows the runtime and the parts that drive a
/// graphics card are not pinned yet: the models' own files are the same.
pub fn parts() -> Vec<Part> {
    let mut parts = Vec::new();
    if cfg!(all(windows, target_arch = "x86_64")) {
        parts.push(Part {
            id: RUNTIME,
            name: "The models' runtime",
            what: "ONNX Runtime 1.32.0, which runs every model: on the processor by itself, on a graphics card with one of the two parts below. Nothing can be studied without it.",
            licence: "Microsoft: MIT License.",
            from: "github.com/microsoft/onnxruntime (its release)",
            needs: &[],
            files: &[Fetched {
                url: "https://github.com/microsoft/onnxruntime/releases/download/v1.32.0/onnxruntime-win-x64-1.32.0.zip",
                sha256: "aaa1d04fcf86bd31e84b90bc0cbde80e632bd0720dcc5d8ba05c4fe45c09d127",
                size: 96_945_131,
                keep: Keep::Members(&[
                    ("onnxruntime-win-x64-1.32.0/lib/onnxruntime.dll", "onnxruntime.dll"),
                    ("onnxruntime-win-x64-1.32.0/lib/onnxruntime_providers_shared.dll", "onnxruntime_providers_shared.dll"),
                ]),
            }],
        });
        parts.push(Part {
            id: ANY_CARD,
            name: "For a graphics card of any make",
            what: "Runs the models on a graphics card through the system's own graphics interface, with nothing of the card's maker: version 0.5.0, the one that was measured working (the one after it fails on NVIDIA cards).",
            licence: "Microsoft: MIT License; its notices list what it is built on.",
            from: "pypi.org/project/onnxruntime-ep-webgpu (Microsoft's package)",
            needs: &[RUNTIME],
            files: &[Fetched {
                url: "https://files.pythonhosted.org/packages/7e/36/9935a5b7851099da3da81d9e04f3021bcc61a5a1039e6c098cf08e5c6607/onnxruntime_ep_webgpu-0.5.0-py3-none-win_amd64.whl",
                sha256: "713b94e7b4ce5979a356e19b926ecba2c0d4ba24f857ef4bbb11c75c37cc5b61",
                size: 13_608_886,
                keep: Keep::Members(&[
                    ("onnxruntime_ep_webgpu/onnxruntime_providers_webgpu.dll", "onnxruntime_providers_webgpu.dll"),
                    ("onnxruntime_ep_webgpu/dxcompiler.dll", "dxcompiler.dll"),
                ]),
            }],
        });
        parts.push(Part {
            id: NVIDIA,
            name: "For an NVIDIA card, through NVIDIA's own libraries",
            what: "About twice as fast on an NVIDIA card as the part for any card, and a gigabyte to fetch. Optional: the part for any card runs NVIDIA cards too.",
            licence: "The provider is Microsoft's, MIT License. NVIDIA's libraries (cuDNN, cuBLAS) are not free software: they come from NVIDIA's own packages under the \"LICENSE AGREEMENT FOR NVIDIA SOFTWARE DEVELOPMENT KITS\", which you accept by fetching them. Trill ships and hosts none of it.",
            from: "github.com/microsoft/onnxruntime (the provider) and pypi.org, NVIDIA's packages nvidia-cudnn-cu13 and nvidia-cublas",
            needs: &[RUNTIME],
            files: &[
                Fetched {
                    url: "https://github.com/microsoft/onnxruntime/releases/download/plugin-ep-cuda/v0.4.0/cuda_ep_cuda13_0.4.0_win-x64.zip",
                    sha256: "d49409c04f271cbc3a5171edea8da2344794c16e17b31731d7eac222c4a5646a",
                    size: 153_590_304,
                    keep: Keep::Members(&[("onnxruntime_providers_cuda.dll", "onnxruntime_providers_cuda.dll")]),
                },
                Fetched {
                    url: "https://files.pythonhosted.org/packages/48/9c/26e6bdd31c97c389aa565b49c916be0dec17dd61dad6cd33fee286d1b946/nvidia_cudnn_cu13-9.28.1.4-py3-none-win_amd64.whl",
                    sha256: "73d874c6bd8a1c75ca040228f77c4e163efe9bab3b65bd85914e2b7334ac68ee",
                    size: 407_065_099,
                    keep: Keep::Under("nvidia/cudnn/bin/", ".dll"),
                },
                Fetched {
                    url: "https://files.pythonhosted.org/packages/12/39/e1b1b4adb8b171894d9a89035df0596d448cf73dc403802d24dbe30cc0be/nvidia_cublas-13.10.0.20-py3-none-win_amd64.whl",
                    sha256: "c310b749eea855b88fafb906de08d6819b87216c6711640771764fea5dcd8546",
                    size: 427_284_863,
                    keep: Keep::Members(&[
                        ("nvidia/cu13/bin/x86_64/cublas64_13.dll", "cublas64_13.dll"),
                        ("nvidia/cu13/bin/x86_64/cublasLt64_13.dll", "cublasLt64_13.dll"),
                    ]),
                },
            ],
        });
    }
    parts.push(Part {
        id: STEMS,
        name: "Stems",
        what: "The model that splits a recording into vocals, drums, bass and the rest (Hybrid Transformer Demucs). Everything a study finds, it finds in the stems.",
        licence: DEMUCS_LICENCE,
        from: "huggingface.co/HRSadeghi/demucs-onnx",
        needs: &[RUNTIME],
        files: &[Fetched {
            url: "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a/htdemucs.onnx",
            sha256: "abaac48bc8fd55fa672268c8f13228b9b00f52f0c4cd10945048982eca4643bc",
            size: 168_683_222,
            keep: Keep::As("htdemucs.onnx"),
        }],
    });
    parts.push(Part {
        id: STEMS_BEST,
        name: "Stems, at their best",
        what: "Four models, one tuned for each stem: the cleanest stems, at four times the work. Optional.",
        licence: DEMUCS_LICENCE,
        from: "huggingface.co/HRSadeghi/demucs-onnx",
        needs: &[RUNTIME],
        files: &[
            Fetched {
                url: "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a/htdemucs_ft_0.onnx",
                sha256: "4738167ecd97779b0b23e21c754f54d65d7b1e1d077a856ba88295b54a35ba4e",
                size: 168_683_222,
                keep: Keep::As("htdemucs_ft_0.onnx"),
            },
            Fetched {
                url: "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a/htdemucs_ft_1.onnx",
                sha256: "5f62c38f565e9a73fe60b1917220119fa7b7fc2c259d059a836a9751f0c01072",
                size: 168_683_222,
                keep: Keep::As("htdemucs_ft_1.onnx"),
            },
            Fetched {
                url: "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a/htdemucs_ft_2.onnx",
                sha256: "f3a57a391d1a26cb1009761d664d44420abe22b7842b7f3147d45a1cdde85a7a",
                size: 168_683_222,
                keep: Keep::As("htdemucs_ft_2.onnx"),
            },
            Fetched {
                url: "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a/htdemucs_ft_3.onnx",
                sha256: "31baa94de72168b84feba38dbae38c99ac6c3877c4184e5892c837a4ea76f4e5",
                size: 168_683_222,
                keep: Keep::As("htdemucs_ft_3.onnx"),
            },
        ],
    });
    parts.push(Part {
        id: BEATS,
        name: "Beats",
        what: "The model that hears where the beats and the bar lines are (Beat This). Without it a study does not run: the pulse of the onsets is what one sound is listened to by.",
        licence: "JKU Linz: \"The code and the published model weights are released under the MIT license. Note that some of the training files are fully copyrighted or under limited Creative Commons licenses, and it is up to the user to assess whether this may impact their use case.\" Converted to ONNX by danigb/beat-this-rs.",
        from: "github.com/danigb/beat-this-rs (its release)",
        needs: &[RUNTIME],
        files: &[Fetched {
            url: "https://github.com/danigb/beat-this-rs/releases/download/model-large/beat_this.onnx",
            sha256: "5f810debe53459b559127fb55bbad40035bb47cc567b20e501670f968c770f02",
            size: 83_162_650,
            keep: Keep::As("beat_this.onnx"),
        }],
    });
    parts.push(Part {
        id: WORDS,
        name: "Words, on a graphics card",
        what: "The model that writes down what is sung (Whisper large-v3-turbo), as a graphics card takes it. Wanted when the models run on a card.",
        licence: WHISPER_LICENCE,
        from: "huggingface.co/onnx-community/whisper-large-v3-turbo",
        needs: &[RUNTIME],
        files: &[
            Fetched {
                url: "https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a/onnx/encoder_model_fp16.onnx",
                sha256: "fdadc70836e6b028fd5e580417c312208dad073d2d01e509e2d127c1373399d8",
                size: 1_274_342_603,
                keep: Keep::As("encoder_model_fp16.onnx"),
            },
            Fetched {
                url: "https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a/onnx/decoder_model_fp16.onnx",
                sha256: "56026785e9d0ae5c93e76c5d5ac9560e0568051ff664fb56c3a9eb32745bd6c5",
                size: 344_003_183,
                keep: Keep::As("decoder_model_fp16.onnx"),
            },
            WORDS_FILES[0],
            WORDS_FILES[1],
            WORDS_FILES[2],
        ],
    });
    parts.push(Part {
        id: WORDS_PROCESSOR,
        name: "Words, on the processor",
        what: "The same model in whole numbers, as a processor takes it. Wanted when the models run on the processor.",
        licence: WHISPER_LICENCE,
        from: "huggingface.co/onnx-community/whisper-large-v3-turbo",
        needs: &[RUNTIME],
        files: &[
            Fetched {
                url: "https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a/onnx/encoder_model_int8.onnx",
                sha256: "e44c0d5cfcc6ad283011602a738fa28dfa1ad7f7540c9503205479072a9cc1ef",
                size: 644_822_094,
                keep: Keep::As("encoder_model_int8.onnx"),
            },
            Fetched {
                url: "https://huggingface.co/onnx-community/whisper-large-v3-turbo/resolve/360ebcde2559d60bb474678be3c1de9ef347d01a/onnx/decoder_model_int8.onnx",
                sha256: "b54c67a80cf31304d75b8d525393e8758c23f848027d4fa916f26077e9f4bde2",
                size: 439_684_851,
                keep: Keep::As("decoder_model_int8.onnx"),
            },
            WORDS_FILES[0],
            WORDS_FILES[1],
            WORDS_FILES[2],
        ],
    });
    parts.push(Part {
        id: WORD_TIMES,
        name: "Word times",
        what: "The model that places each word where it is sung (Omnilingual ASR, CTC, 300M). Without it the words have only the times of the phrases they are in.",
        licence: "Meta: \"Omnilingual ASR code and models are released under the Apache 2.0.\" Converted to ONNX by the sherpa-onnx project.",
        from: "huggingface.co/csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-2025-11-12",
        needs: &[RUNTIME],
        files: &[
            Fetched {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-2025-11-12/resolve/98ab94b2a45a0d13f852c80ae77c52766ac1ea25/model.onnx",
                sha256: "25f6faecf3f541626e6d3b793103d9544c2f52b0f4f40373b4e2720ac3ff77d5",
                size: 1_302_628_362,
                keep: Keep::As("model.onnx"),
            },
            Fetched {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-2025-11-12/resolve/98ab94b2a45a0d13f852c80ae77c52766ac1ea25/tokens.txt",
                sha256: "a7a044c52cb29cbe8b0dc1953e92cefd4ca16b0ed968177b6beab21f9a7d0b31",
                size: 86_423,
                keep: Keep::As("tokens.txt"),
            },
        ],
    });
    let _ = WHISPER;
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_is_pinned_and_says_whose_it_is() {
        let parts = parts();
        let ids: Vec<&str> = parts.iter().map(|part| part.id).collect();
        for wanted in [STEMS, STEMS_BEST, BEATS, WORDS, WORDS_PROCESSOR, WORD_TIMES] {
            assert!(ids.contains(&wanted), "{wanted} is not offered");
        }
        for part in &parts {
            assert!(!part.files.is_empty() && part.size() > 0, "{}", part.id);
            assert!(part.licence.len() > 20 && part.from.contains('.') && part.what.ends_with('.'), "{}", part.id);
            for file in part.files {
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
            for needed in part.needs {
                assert!(cfg!(not(windows)) || ids.contains(needed), "{} needs {needed}, which is not offered", part.id);
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
    }
}
