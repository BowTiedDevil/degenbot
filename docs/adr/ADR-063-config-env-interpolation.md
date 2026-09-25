# ADR-063: The file is portable, the secret is not — `${env:NAME}` expansion for string-valued config keys

**Status: proposed** (2026-09-25). **Implementation is deliberately out of
scope for the ADR-062 epic and gets its own epic; nothing in this record is
ratified by ADR-062's acceptance.** Companion to **ADR-062** (one operator file,
four layers), which makes the operator file the base layer for node endpoints and
therefore the place a credential would naturally end up. Predecessors: ADR-062
(D1 layer table, D12 redaction/reporting), ADR-051 (the console owns the operator
surface), ADR-006 D5 (one `Bot` per chain).

## Context

ADR-062 removes the "export the whole URL or don't use the file" ergonomics, and
with it the reason some operators kept their endpoints out of the file entirely.
That leaves two properties in tension:

- **The file is portable.** The devcontainer bind-mounts the host's
  `~/.config/degenbot/config.toml`, so the container reads the *host's*
  deployment and overrides individual endpoints through the environment. The same
  file is copied between a laptop, a staging box, and a production host.
- **The credential is not portable.** Production endpoints carry an API key in
  the path or query (`https://…/v2/<key>`, `?api_key=…`), it is per-account and
  per-environment, and a file created with default permissions is not a secret
  store.

ADR-062 already settles half of this: `config show --resolved`, error messages,
and logs never print credentials, and the docs tell operators to `chmod 600` the
file. What it does not settle is the *authoring* side. Without an answer, the two
remaining options are both bad: write the key into the file anyway, or keep the
whole URL in the environment and lose the file layer for the one value that most
needs it.

## Decision

**D1 — One expansion pass, one escape.** `${env:NAME}` in any string-valued
config value expands to that environment variable at load time. The expansion set
is exactly `env`. It applies uniformly to `string`, `path`, and `str_map` values
— one rule for every key, because a per-key exception is a second dialect an
operator has to remember and the next key added would need the exception too.

**D2 — An unset variable is a load error, never an empty string.** A `${env:NAME}`
whose variable is unset or empty fails the load with a message naming the config
key, the variable, and the file, aggregated with the loader's other problems.
Expanding to empty instead trades a precise error for a confusing one, and can
produce a valid-but-wrong value (an empty path segment, a query parameter with no
value).

**D3 — `${env:NAME}` is the only token; everything else is literal.** The
expansion set is closed, and closed by *not interpreting* rather than by raising
errors: a `${file:…}`, `${env:NAME:-default}`, or `${env:${env:OTHER}}` sequence
is not a token and passes through as literal text, so no additional surface
exists to audit and no value an operator did not intend as a token is refused.
`\${` escapes a literal `${`. The realistic mistake — a well-formed token naming
a variable that is not set — is already a load error under D2, which is where the
diagnostic belongs; a value that never intended to be a token at all (a file
produced by `envsubst`, Helm, or any other templating tool) is left alone.

**D4 — Interpolation is portability, not precedence.** An expanded file value is
still a `Source::File` value: the `DEGENBOT_RPC_HTTP_CHAINID_<id>` family still
outranks it, and an explicit `--node-http` still outranks both. Provenance records
the layer that supplied the *unexpanded* text, so `config show --resolved` can
report "from file (expanded from `DEGENBOT_RPC_KEY`)" without pretending the
environment supplied the endpoint.

**D5 — Expansion runs after parsing, before semantic validation.** A URL that is
only well-formed once expanded is validated as the expanded value, so scheme and
reachability checks see what the bot will actually dial. Validation failures name
the *expanded* value with credentials redacted, never the raw template.

**D6 — Redaction is a reporting rule.** Userinfo (`https://user:pass@host`) and
credential-bearing query parameters are stripped from every rendered value:
`degenbot config show --resolved`, error text, and log lines. The file on disk
keeps exactly what the operator wrote — redaction exists because the *output* of
a diagnostic is routinely pasted into a terminal scrollback, an issue tracker, or
a CI log, not because the file is a secret store. ADR-062's `chmod 600` guidance
remains the storage-side advice.

**D7 — Non-goals.** No secret-store integration, no encrypted config file, no
per-key credential object, no `.env` file loading by the config layer. If
interpolation proves insufficient in practice, those are the next candidates and
each needs its own decision.

**D8 — Gates.** Unit tests for expansion, refusal, escaping, and redaction in
`degenbot-config`; a loader-level test that an unset variable fails the load; a
redaction test over `config show --resolved` output; and a documentation section
showing the portability pattern (one file, `${env:…}` placeholders, per-machine
credentials in the environment).

## Consequences

An operator writes `http = { 1 = "${ALCHEMY_MAINNET_RPC}" }` in one file; the
devcontainer exports a container-local key, the production host exports the
production key, and the file is identical in both. A missing credential fails the
boot naming the key that wanted it, which is the first time an operator learns
about the mistake at the moment it matters.

The cost is a general string-expansion pass in the loader — the first place
`degenbot-config` reads the environment for something other than a declared key
name, and the reason the expansion lives in the crate that already owns all
environment access rather than in a caller.
