# 🔪 ThanosTokenKiller

> **Dein Coding-Agent liest 3.605 Tokens Testausgabe, um zu erfahren, dass alles grün ist.**
> `ttk` macht daraus 62 – und hebt die Originalausgabe auf, falls doch jemand nachschauen will.

```console
$ ttk run -- cargo test
[test:cargo] status=pass
passed=143 failed=0 ignored=0 duration=6.13s cmd="cargo test" exit=0
raw=cap://cap_01KZ48HZHWBMPN63ETEXQDT001

ttk: ~3605 → ~62 Tokens (-98%)
```

---

## ✨ Warum ttk?

- **Bis zu 99 % weniger Tokens** bei Tests, Builds, Logs und `git`
- **Nichts geht verloren** – die vollständige Ausgabe bleibt lokal, ein Befehl holt sie zurück
- **Fehler bleiben Fehler** – Dateipfade, Zeilennummern, Exit-Codes und Assertions überleben wortgleich
- **Läuft offline** – keine Cloud, kein API-Key, keine Telemetrie, kein Account
- **Ein einzelnes Binary** – Linux und Windows, keine Laufzeitumgebung
- **Secrets bleiben bei dir** – erkannte Zugangsdaten werden ersetzt, bevor ein Modell sie sieht

---

## 📊 Was es spart

| Ausgabe | Modus | vorher | nachher | gespart |
|---|---|---:|---:|---:|
| Application-Log, 501 Zeilen | balanced | 14.536 | 165 | **99 %** |
| `cargo test`, 149 Tests | safe | 3.605 | 62 | **98 %** |
| JSON-Array, 200 Objekte | balanced | 4.211 | 112 | **97 %** |
| pytest, 818 Tests, 2 Failures | safe | 759 | 180 | **76 %** |
| `git status`, 4 Dateien | safe | 244 | 113 | 54 % |
| `go test`, 3 Tests, 1 Failure | safe | 122 | 75 | 39 % |
| `git diff`, 2 Dateien | safe | 330 | 204 | 38 % |
| jest, 813 Tests, 1 Failure | safe | 157 | 112 | 29 % |

Je mehr Ausgabe, desto mehr Ersparnis – `ttk` behält immer ungefähr gleich viel
Signal, egal wie viel Rauschen drumherum steht.

<sub>Je eine reale Messung durch die CLI, mit dem eingebauten Token-Schätzer (`~`).</sub>

---

## 📦 Features

- 🧩 Versteht **pytest, cargo test, jest, vitest, go test, rustc, git, Logs, JSON und grep**
- 📦 Jede Ausgabe landet in einer **Kapsel** – abrufbar von der Einzeilen-Zusammenfassung
  bis zum byte-genauen Original
- 🛡️ Prüft **vor** jeder Kürzung, dass kein Fehler, Pfad oder Wert verschwindet –
  sonst bleibt die Ausgabe unangetastet
- 🎚️ **Modi** von `observe` (misst nur) über `safe` (Standard) bis `maximum`
- 📈 `ttk stats` zeigt, was du bisher gespart hast
- 🤖 `ttk install` richtet Claude Code und Codex automatisch ein

---

## 🚀 Geplant

- ⬜ Proxy für OpenAI- und Anthropic-APIs – `ttk` für den kompletten Agenten, nicht nur die Shell
- ⬜ Kontext-Compiler, der pro Anfrage nur das Nötige zusammenstellt
- ⬜ Symbolindex per tree-sitter statt ganzer Dateien
- ⬜ Projektgedächtnis für Subagenten
- ⬜ MCP-Tool-Schemata nur laden, wenn sie gebraucht werden
- ⬜ Plugin-System für eigene Parser
- ⬜ Exakte Tokenzählung statt Schätzung

---

## 🔧 Installation

Du brauchst nur [Rust](https://rustup.rs) (1.90+).

```bash
# 1. Repository klonen
git clone https://github.com/Bauvater/ThanosTokenKiller.git
cd ThanosTokenKiller

# 2. Bauen
cargo build --release

# 3. Einrichten – wählt deinen Agenten und legt ttk in den PATH
./target/release/ttk install

# 4. Neues Terminal öffnen
ttk doctor
```

---

## ⚡ Loslegen

```bash
ttk run -- cargo test               # Befehl ausführen, kompakte Ausgabe erhalten
ttk stats                           # was hat es bisher gespart?
ttk retrieve <capsule> --level 4    # die vollständige Originalausgabe
ttk help                            # alle Befehle
```

Auch als Filter:

```bash
kubectl logs deploy/api --tail=5000 | ttk compile
```

Der Exit-Code des Befehls wird unverändert durchgereicht – `&&`-Ketten und
Skripte laufen weiter wie vorher.

---

## 🤖 Für deinen Agenten

```console
$ ttk install

Which coding agent should learn to use ttk?
  1) Claude Code  (CLAUDE.md)
  2) Codex        (AGENTS.md)
  3) All of them
```

Das trägt einen kurzen Block in `CLAUDE.md` bzw. `AGENTS.md` ein: dass Befehle
über `ttk run --` laufen sollen, wie die kompakte Ausgabe zu lesen ist und wie
der Agent sich die vollständige Ausgabe zurückholt, statt einen teuren Befehl
noch einmal auszuführen.

Deine eigenen Regeln bleiben unangetastet – der Block steht zwischen Markern und
wird beim nächsten Mal an Ort und Stelle aktualisiert.

Für Skripte:

```bash
ttk install --target all --scope project --yes
ttk install --target all --yes --no-path     # ohne PATH anzufassen
```

---

## 📈 Deine Bilanz

```console
$ ttk stats
tokens saved            ~1284933  (71.4% of ~1799204)
commands run                 412
sessions                      36

top commands
  cargo                 ~702118 saved  (188 runs)
  pytest                ~381044 saved  (96 runs)
  git                    ~14119 saved  (74 runs)
```

<sub>Beispielausgabe – zeigt das Format, nicht deine Zahlen. `ttk stats --json` für Skripte.</sub>

---

## 🤝 Beitragen

Pull Requests sind willkommen. Einen neuen Parser hinzufügen heißt:
`ttk_compilers::Compiler` implementieren und in `registry()` eintragen – den Rest
übernimmt die Prüfschicht.

```bash
cargo test
cargo clippy --all-targets
```

---

## 📄 Lizenz

MIT

---
---

# 🔪 ThanosTokenKiller

> **Your coding agent reads 3,605 tokens of test output to learn that everything passed.**
> `ttk` turns that into 62 – and keeps the original around in case anyone needs a closer look.

```console
$ ttk run -- cargo test
[test:cargo] status=pass
passed=143 failed=0 ignored=0 duration=6.13s cmd="cargo test" exit=0
raw=cap://cap_01KZ48HZHWBMPN63ETEXQDT001

ttk: ~3605 → ~62 tokens (-98%)
```

---

## ✨ Why ttk?

- **Up to 99% fewer tokens** on tests, builds, logs and `git`
- **Nothing is lost** – the full output stays on disk, one command brings it back
- **Errors stay errors** – file paths, line numbers, exit codes and assertions survive word for word
- **Runs offline** – no cloud, no API key, no telemetry, no account
- **A single binary** – Linux and Windows, no runtime to install
- **Secrets stay yours** – detected credentials are replaced before a model sees them

---

## 📊 What it saves

| Output | Mode | before | after | saved |
|---|---|---:|---:|---:|
| application log, 501 lines | balanced | 14,536 | 165 | **99%** |
| `cargo test`, 149 tests | safe | 3,605 | 62 | **98%** |
| JSON array, 200 objects | balanced | 4,211 | 112 | **97%** |
| pytest, 818 tests, 2 failures | safe | 759 | 180 | **76%** |
| `git status`, 4 files | safe | 244 | 113 | 54% |
| `go test`, 3 tests, 1 failure | safe | 122 | 75 | 39% |
| `git diff`, 2 files | safe | 330 | 204 | 38% |
| jest, 813 tests, 1 failure | safe | 157 | 112 | 29% |

The more output, the bigger the win – `ttk` keeps roughly the same amount of
signal no matter how much noise surrounds it.

<sub>One real measurement each, taken through the CLI with the built-in token estimator (`~`).</sub>

---

## 📦 Features

- 🧩 Understands **pytest, cargo test, jest, vitest, go test, rustc, git, logs, JSON and grep**
- 📦 Every output goes into a **capsule** – from a one line summary all the way
  down to the byte exact original
- 🛡️ Checks **before** shortening anything that no error, path or value disappears –
  otherwise the output is left alone
- 🎚️ **Modes** from `observe` (measure only) through `safe` (default) to `maximum`
- 📈 `ttk stats` shows what you have saved so far
- 🤖 `ttk install` sets up Claude Code and Codex for you

---

## 🚀 Planned

- ⬜ Proxy for the OpenAI and Anthropic APIs – `ttk` for the whole agent, not just the shell
- ⬜ Context compiler that assembles only what each request needs
- ⬜ tree-sitter symbol index instead of whole files
- ⬜ Project memory for subagents
- ⬜ Load MCP tool schemas only when they are actually needed
- ⬜ Plugin system for your own parsers
- ⬜ Exact token counting instead of estimation

---

## 🔧 Installation

All you need is [Rust](https://rustup.rs) (1.90+).

```bash
# 1. clone the repository
git clone https://github.com/Bauvater/ThanosTokenKiller.git
cd ThanosTokenKiller

# 2. build
cargo build --release

# 3. set up – picks your agent and puts ttk on your PATH
./target/release/ttk install

# 4. open a new terminal
ttk doctor
```

---

## ⚡ Getting started

```bash
ttk run -- cargo test               # run a command, get compact output
ttk stats                           # what has it saved so far?
ttk retrieve <capsule> --level 4    # the complete original output
ttk help                            # every command
```

Works as a filter too:

```bash
kubectl logs deploy/api --tail=5000 | ttk compile
```

The command's exit code is passed through unchanged, so `&&` chains and scripts
keep working exactly as before.

---

## 🤖 For your agent

```console
$ ttk install

Which coding agent should learn to use ttk?
  1) Claude Code  (CLAUDE.md)
  2) Codex        (AGENTS.md)
  3) All of them
```

This adds a short block to `CLAUDE.md` or `AGENTS.md`: run commands through
`ttk run --`, how to read the compact output, and how to pull the full output
back instead of re-running an expensive command.

Your own rules stay untouched – the block sits between markers and is updated in
place next time.

For scripts:

```bash
ttk install --target all --scope project --yes
ttk install --target all --yes --no-path     # without touching PATH
```

---

## 📈 Your numbers

```console
$ ttk stats
tokens saved            ~1284933  (71.4% of ~1799204)
commands run                 412
sessions                      36

top commands
  cargo                 ~702118 saved  (188 runs)
  pytest                ~381044 saved  (96 runs)
  git                    ~14119 saved  (74 runs)
```

<sub>Example output – shows the format, not your numbers. Use `ttk stats --json` for scripts.</sub>

---

## 🤝 Contributing

Pull requests welcome. Adding a parser means implementing
`ttk_compilers::Compiler` and registering it in `registry()` – the safety layer
takes care of the rest.

```bash
cargo test
cargo clippy --all-targets
```

---

## 📄 Licence

MIT
