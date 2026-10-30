# voice providers

One file per speech-recognition service, each implementing `voice::Provider`
(`transcribe`, `status`). The command vocabulary is passed as context so the
service prefers `disarm` over sound-alikes.

| File | Service | Notes |
|------|---------|-------|
| `deepgram.rs` | Deepgram `nova-3`, `POST /v1/listen` | reports the project balance; the one in use here |
| `openai.rs` | OpenAI `gpt-4o-transcribe`, multipart upload | no balance in the API |

Nothing is decoded on the Orin (see the project memory on this); an
Anthropic key is recognised and ignored, since Claude does not transcribe.
