# Stimmwerk LLM Mock-Fixtures (Etappe 7)

Kanonische HTTP-Antwortkörper für `testsrv` (Mock-Server, 127.0.0.1). Keine echten Keys, keine Netzwerkzugriffe. Test-Refs = docs/stimmwerk/etappe7-card.md §10 (`#`) bzw. llm-etappe7.md (Namen).

Konventionen:
- `*.json` = Body, `*.headers` = Zusatz-Header (`retry-after: 0` nur für schnelle Tests; real 1–30 s).
- Success-Content = `prosa/kapitel1.expected.txt` als Prose→Script-JSON (`{cast, lines}`, 34 Zeilen, Attribution entfällt laut Fixture). Cast aus `prosa/cast.toml`. Damit gilt: `prose_fixture_e2e` (#34) → Recall/Precision 100 %.
- `200_length` und `200_malformed_content`: Content ist absichtlich kein gültiges JSON.
- Modell: OpenAI `fixture-model`, Anthropic `claude-haiku-5-5`.

## openai/ (Chat Completions, `Authorization: Bearer`)

| Datei | Test | Erwartetes Client-Verhalten |
|---|---|---|
| `200_success.json` | #34 `prose_fixture_e2e`, llm `chat_200` | Ok. `choices[0].message.content` = Payload; `finish_reason=stop`; `parse_json` → `validate` leer → `to_script` → Score 100 %. hits=1. |
| `200_length.json` | llm `finish_reason=length` (neu) | Err "Ausgabe abgeschnitten" (kein Teil-JSON weitergeben). Kein HTTP-Retry. hits=1. |
| `200_malformed_content.json` | #31/#32 Prose-Retry, llm `error_200_non_json`-Variante | `finish_reason=stop`, aber Content unparsbar → JSON-Parse-Fehler (kein Panic). `prose::run` Retry 1× mit Fehlerliste; bei erneutem Fehler Err. hits=2 im Prose-Lauf, sonst 1. |
| `401.json` | llm `error_401_message` | Err mit `error.message` ("Incorrect API key provided."). Kein Key im Fehlertext. hits=1, kein Retry. |
| `429.json` + `429.headers` | llm (OpenAI-429, neu) | Err "LLM-Fehler 429: Rate limit reached…". Kein Retry (OpenAI-Pfad unverändert, Retry nur Anthropic). `retry-after` wird ignoriert. hits=1. Offen: ob OpenAI-Pfad 429 auch retryen soll. |
| `500.json` | llm `error_500_plain` | Err `LLM-Fehler 500: internal error`. Kein Retry. hits=1. |
| `500_gpu_timeout.json` | llm `error_500_gpu_dead` | Err-Variante `GPU_DEAD` (nur lokaler Endpoint, `gpu_error`). Kein Retry. hits=1. |

## anthropic/ (Messages API, `x-api-key` + `anthropic-version: 2023-06-01`)

| Datei | Test | Erwartetes Client-Verhalten |
|---|---|---|
| `200_success.json` | #4/#34, llm `parse_mixed_blocks` | Ok. `content[]` type=text → `.text` = Payload; `stop_reason=end_turn`. Prose-Score 100 %. hits=1. |
| `200_max_tokens.json` | #7 `parse_anthropic_max_tokens_err`, llm `stop_reason_max_tokens` | Err "Ausgabe abgeschnitten". Kein Retry. hits=1. |
| `200_refusal.json` | #8 `parse_anthropic_refusal_err` | Err (refusal). `content` leer. Kein Retry, kein Prose-Retry-Loop. hits=1. |
| `401.json` | llm `error_401` | Err `authentication_error: invalid x-api-key`. Kein Retry. hits=1. |
| `429.json` + `429.headers` | #9 `anthropic_429_retry`, #13 `retry_after_honored`, llm `error_429_retry_after_then_ok` | Mock `[429, 429, 200]` → Ok, hits=3, Backoff 1 s/3 s (Test-Override). Mock `[429]` ×3 → Err `rate_limit_error`. `retry-after` honoriert. |
| `529.json` | #10 `anthropic_529_retry_exhausted`, llm `error_529_exhausted` | 3× 529 → Err mit `overloaded_error`, hits=3 (max 2 Retries). |

## Mock-Ketten (Beispiele)

- #9: `serve([err(429,…anthropic/429), err(429,…), ok(anthropic/200_success)])` → hits 3, Ok.
- #10: `serve([err(529,…)])` (Wiederholung via min(i,len-1)) → hits 3, Err.
- #31: `serve([ok(openai/200_malformed_content), ok(valid payload)])` → hits 2, Ok, 2. Request enthält Fehlerliste.
- #32: `serve([ok(openai/200_malformed_content)])` → hits 2, Err.
- #34: `serve([ok(openai/200_success)])` (oder anthropic/200_success) → `run()` → Score 100 %.

Nicht abgedeckt (bewusst, nicht angefordert): Anthropic 500/400, OpenAI 400, Streaming, Tool-Use-Antworten.
