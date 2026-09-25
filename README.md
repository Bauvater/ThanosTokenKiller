<div align="center">

# 🔪 ThanosTokenKiller

**Shrinks what your coding agent reads — tests, builds, git, logs —<br>
and keeps every byte of the original one command away.**

*And it gets better every time your agent uses it.*

[![CI](https://github.com/Bauvater/ThanosTokenKiller/actions/workflows/ci.yml/badge.svg)](https://github.com/Bauvater/ThanosTokenKiller/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Bauvater/ThanosTokenKiller?include_prereleases)](https://github.com/Bauvater/ThanosTokenKiller/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#-license)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey)
![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-orange)

**English** · [Deutsch](#-deutsch)

</div>

---

Your coding agent reads **3,605 tokens** of test output to learn that everything
passed. `ttk` turns that into **62**, and keeps the original in case anyone needs a
closer look:

```console
$ ttk run -- cargo test
[test:cargo] status=pass
passed=143 failed=0 ignored=0 duration=6.13s cmd="cargo test" exit=0
raw=cap://cap_01KZ48HZHWBMPN63ETEXQDT001

ttk  ~3605 → ~62 tokens  ███████████████░  -98%  cap://cap_01KZ48HZ…
```

## What it does

`ttk` sits between your coding agent (Claude Code, Codex, …) and the shell.

1. **It compiles output.** Test runners, compilers, `git`, logs and JSON are parsed
   and reduced to what matters: status, counts, every failure with its file and
   line. The command's exit code passes through unchanged.
2. **It never loses anything.** The byte-exact original goes into a local
   *capsule*. One command brings back all of it, a line range, or a search hit.
   The agent never has to re-run an expensive command just to see more.
3. **It learns.** Your agent marks the lines it never wants to see again, and
   `ttk` turns them into safe, reviewable filter rules. Every lesson makes every
   later run smaller, in this project or, with global filters, in all of them.
   The more you use it, the less noise your agent pays for.
4. **It keeps score.** Every run in every project lands in one ledger. `ttk gain`
   tells you how many tokens you have saved, today and in total.

A quality firewall checks every shortening **before** it is used. If an error, a
path, a line number or an exit code would disappear, `ttk` falls back to the
original output. Nothing leaves your machine: no cloud, no API key, no account.

## Contents

- [Why ttk?](#-why-ttk)
- [Installation](#-installation)
- [Quick start](#-quick-start)
- [It gets better the more you use it](#-it-gets-better-the-more-you-use-it)
- [What it saves](#-what-it-saves)
- [Your numbers: `ttk gain`](#-your-numbers-ttk-gain)
- [The global folder](#-the-global-folder)
- [Setting up your agent](#-setting-up-your-agent)
- [Features](#-features) · [Roadmap](#-roadmap) · [Contributing](#-contributing) · [License](#-license)

---

## ✨ Why ttk?

- **Up to 99% fewer tokens** on tests, builds, logs and `git`
- **Nothing is lost.** The full output stays on disk, and one command brings it back
- **Errors stay errors.** File paths, line numbers, exit codes and assertions survive word for word
- **Self-improving.** Your agent marks the junk once and never pays for it again
- **Repetition is free.** Run the same command twice and the second run costs a pointer
- **Secrets stay yours.** Detected credentials are replaced before a model sees them
- **Runs offline.** No cloud, no API key, no telemetry, no account
- **One binary.** Windows and Linux, no runtime to install
- **One running total.** `ttk gain` covers every project, not twenty separately

---

## 🔧 Installation

### Windows: installer (recommended)

Download **`ttk-setup-<version>-x86_64-windows.exe`** from the
[releases page](https://github.com/Bauvater/ThanosTokenKiller/releases) and
double-click it. It is one file and needs no administrator rights.

- installs `ttk.exe` to `%LOCALAPPDATA%\Programs\ttk`
- **replaces any older ttk** on your `PATH` and puts this one first
- creates the [global folder](#-the-global-folder) `%APPDATA%\ttk`
- optionally teaches Claude Code to use it (global `CLAUDE.md`)
- adds Start menu shortcuts and an entry under *Settings → Apps*, so it
  uninstalls like any other program

Unattended: `ttk-setup.exe --yes`. For all options: `ttk-setup.exe --help`.

### Script install

```bash
# Linux
curl -fsSL https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.ps1 | iex
```

Both download the release for your platform, **verify its SHA-256** against the
published checksum, and stop if they disagree. Then `ttk setup` walks you
through the workspace, your agent and `PATH`, and asks before each step.

### From source

Requires [Rust](https://rustup.rs) 1.90 or newer.

```bash
git clone https://github.com/Bauvater/ThanosTokenKiller.git
cd ThanosTokenKiller
cargo build --release
./target/release/ttk setup
```

To build the Windows installer yourself, run
`powershell -File scripts\build-installer.ps1`. The result is in `dist\`.

---

## ⚡ Quick start

```bash
ttk                                 # overview of the essentials
ttk run -- cargo test               # run a command, get compact output
ttk read --outline src/main.rs      # a file's declarations instead of the file
ttk retrieve <capsule> --level 4    # the complete original output
ttk suggest                         # noise that keeps coming back
ttk learn --last                    # teach the filter what was noise
ttk gain                            # tokens saved, across ALL projects
ttk global --open                   # open the global folder
ttk doctor                          # check the installation
ttk help --all                      # every command and flag
```

Output can be piped in too, and the exit code always passes through unchanged:

```bash
kubectl logs deploy/api --tail=5000 | ttk compile
```

Running the same command twice does not cost twice. Identical output collapses
to `[repeat] identical to cap://…`, and nearly identical output to a `[delta]` of
what changed. Error lines are kept in both cases, so a repeated *failure* still
shows the failure.

---

## 🧠 It gets better the more you use it

Other tools ship a fixed list of hand-written filters. Somebody notices that
`npm install` prints forty deprecation warnings, and somebody writes a regex.
That scales exactly as far as that person's patience.

`ttk` turns this around: **your agent writes the filters.** It has just read the
output and knows better than any maintainer which parts were worthless, so it
marks them:

```console
$ ttk run -- npm install
npm WARN deprecated inflight@1.0.6: This module is not supported anymore
npm WARN deprecated glob@7.2.3: This module is not supported anymore
added 412 packages in 9s

$ ttk learn --last <<'EOF'
<filter-trash>
npm WARN deprecated inflight@1.0.6: This module is not supported anymore
npm WARN deprecated glob@7.2.3: This module is not supported anymore
</filter-trash>
added 412 packages in 9s
EOF

✓ 1 rule(s) in force for this project
```

From then on no model sees those lines again, on **every** `npm install` and at
any version number. Every lesson adds to what the filter knows, and the knowledge
builds up in three places:

| Where | Applies to | How |
|---|---|---|
| `.ttk/learned-filters.json` | this project, and everyone who clones it | `ttk learn --last` (commit the file) |
| `%APPDATA%\ttk\filters\` | **every** project on this machine | `ttk learn --last --user`, or drop in any rule file |
| `ttk suggest` | whatever keeps coming back | drafts a lesson from what ttk has already seen, for you to review |

The loop keeps tightening. `ttk suggest` spots recurring noise, your agent confirms
it, the rule fires on every later run, and `ttk rules` shows what each rule has
actually saved. A rule that never fires can be pruned (`ttk rules prune`).

Output that cannot be described line by line, such as a boxed banner, is learned
as one **block rule** that removes the whole run or nothing. Tags beyond plain
deletion:

```text
<filter-trash>…</filter-trash>          delete these lines
<filter-fold as="what it was">…</…>     replace the run with one summary line
<filter-only>…</filter-only>            these are the ONLY lines worth keeping
<filter-keep>…</filter-keep>            this was removed and should not have been
```

### Why learning is safe

- **Rules are token templates, not regexes.** Words have to match exactly. Only
  volatile values (`{n}`, `{ver}`, `{hex}`, `{t}`, `{size}`, `{path}`, `{url}`,
  `{uuid}`) are wildcards, so you can read a rule out loud.
- **Whatever you do not mark is a counter-example.** A rule that would also match
  an unmarked line is refused, and ttk tells you why.
- **Errors are never filtered.** Exit codes, assertions, stack traces and line
  references are recognised by the same check the quality firewall uses.
- **The firewall has the last word.** A filtered result is reviewed like any other
  shortening, and the original is already in a capsule before the filter runs.
- **Mistakes undo themselves.** Mark a wrongly removed line with `<filter-keep>` and
  teach again, and the rule responsible is retired.

```bash
ttk rules                     # what has been learned, ranked by tokens saved
ttk rules show <id>           # one rule in detail
ttk rules disable|forget <id> # retire or delete a rule
ttk filter --explain          # dry run over stdin, changes nothing
```

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

The more output there is, the bigger the win. `ttk` keeps roughly the same amount
of signal no matter how much noise surrounds it. Learned filters come on top of
these numbers.

<sub>One real measurement each, taken through the CLI with the built-in token estimator (`~`). Single measurements, not a benchmark suite.</sub>

---

## 📈 Your numbers: `ttk gain`

Every `ttk run`, in every repository, is counted in **one** ledger, so a single
command answers "how much has this saved me?" from anywhere:

```console
$ ttk gain
╭────────────────────────────────────────────────────────────────╮
│  ◆ ttk  token savings · all projects                   v0.1.0  │
╰────────────────────────────────────────────────────────────────╯

  TOKENS SAVED
  ~1 284 933
  █████████████████████████░░░░░░░░░░░  71.4%  of ~1 799 204 tokens never reached a model

  period            saved  runs  rate
  today           ~41 877    23   78%
  last 7 days    ~212 530   118   72%
  last 30 days   ~803 114   301   71%
  all time     ~1 284 933   412   71%

▌ last 14 days  ▂▅▃█▁▁▄▆▃▇▂▁▅▆
  today      ███████████████░░░░░░░░░░░░░      ~41 877  23 runs
  Thu 09-24  ████████████████████░░░░░░░░      ~55 012  31 runs
  …

▌ top commands
  command     saved                    rate  runs
  cargo    ~702 118  ████████████████   73%   188
  pytest   ~381 044  █████████░░░░░░░   69%    96
```

<sub>Example output. It shows the format, not your numbers.</sub>

| Command | Shows |
|---|---|
| `ttk gain` | every project: total, today, 7 and 30 days, daily chart, top commands and projects |
| `ttk gain --project` | only the project you are in |
| `ttk gain --days 30` | a longer daily chart |
| `ttk gain --session latest` | one session in detail |
| `ttk gain --json` | the same for scripts |
| `ttk stats` | lifetime detail for this workspace (transformers, capsules, fallbacks) |

---

## 🌍 The global folder

Everything that is not tied to one project lives in one place: `%APPDATA%\ttk`
on Windows, `~/.config/ttk` elsewhere. `ttk global` shows what is inside, and
`ttk global --open` opens it.

```text
%APPDATA%\ttk\
├── filters\                global filters: EVERY *.json rule file here applies in every project
│   └── learned-filters.json   written by `ttk learn --user`
├── usage.jsonl             every token saved, from every project (what `ttk gain` adds up)
├── config.toml             optional user configuration
└── README.txt              what all of this is
```

Rules from `ttk rules export` can be dropped straight into `filters\`. The folder
holds local paths and never leaves your machine. `TTK_GLOBAL_HOME` moves the whole
folder, `TTK_USAGE=0` switches the ledger off, and `ttk usage forget --all` empties it.

---

## 🤖 Setting up your agent

```console
$ ttk install

Which coding agent should learn to use ttk?
  1) Claude Code  (CLAUDE.md)
  2) Codex        (AGENTS.md)
  3) All of them
```

This adds one marked block to `CLAUDE.md` or `AGENTS.md`. The block tells the
agent to run commands through `ttk run --`, how to read the compact output, how to
pull the full output back instead of re-running a command, and how to teach the
filter. Your own instructions stay untouched, and the block is updated in place
next time.

```bash
ttk install --target all --scope project --yes   # non-interactive
ttk install --target claude --scope global --yes # every project, via ~/.claude/CLAUDE.md
ttk install --compact                            # ~200-token block instead of ~800
```

---

## 📦 Features

- 🧩 Understands **pytest, cargo test, jest, vitest, go test, rustc, git, logs, JSON and grep**
- 📦 Every output goes into a **capsule**, retrievable from a one-line summary down to the byte-exact original
- 🛡️ A **quality firewall** checks every shortening before it is used, and falls back to the original if anything important would be lost
- 🧠 **Self-improving filters** with line and block rules, fold summaries, whitelists and automatic suggestions
- 🌍 **Global filters** that follow you into every project
- 🔁 **Repeat and delta suppression** for commands *and* file reads
- 📂 `ttk read --outline`: a file's declarations instead of the whole file
- 📈 `ttk gain`: one running total across every project, with a daily chart
- 🔐 Secret detection. Credentials are replaced before a model sees them
- 🎚️ **Modes** from `observe` (measure only) through `safe` (default) to `maximum`
- 🎨 Coloured, aligned terminal output that switches to plain text in a pipe (`NO_COLOR`, `TTK_ASCII=1`)
- 🪟 A one-file Windows installer that replaces old versions and uninstalls cleanly

## 🚀 Roadmap

- ⬜ Proxy for the OpenAI and Anthropic APIs: `ttk` for the whole agent, not just the shell
- ⬜ Context compiler that assembles only what each request needs
- ⬜ tree-sitter symbol index instead of whole files
- ⬜ Project memory for subagents
- ⬜ Load MCP tool schemas only when they are needed
- ⬜ Plugin system for your own parsers
- ⬜ Exact token counting instead of estimation
- ⬜ Block rules that generalise (today they need an exact run in an exact order)

## 🤝 Contributing

Pull requests are welcome. To add a parser, implement `ttk_compilers::Compiler`
and register it in `registry()`. The safety layer takes care of the rest.

```bash
cargo test
cargo clippy --all-targets
cargo fmt --all --check
```

## 📄 License

[MIT](LICENSE)

---
---

<div align="center">

# 🇩🇪 Deutsch

**Schrumpft, was dein Coding-Agent liest (Tests, Builds, git, Logs),<br>
und hält jedes Byte des Originals einen Befehl entfernt bereit.**

*Und es wird mit jeder Nutzung besser.*

[English](#-thanostokenkiller) · **Deutsch**

</div>

Dein Coding-Agent liest **3.605 Tokens** Testausgabe, nur um zu erfahren, dass
alles grün ist. `ttk` macht daraus **62** und hebt das Original auf, falls doch
jemand genauer nachsehen will:

```console
$ ttk run -- cargo test
[test:cargo] status=pass
passed=143 failed=0 ignored=0 duration=6.13s cmd="cargo test" exit=0
raw=cap://cap_01KZ48HZHWBMPN63ETEXQDT001

ttk  ~3605 → ~62 tokens  ███████████████░  -98%  cap://cap_01KZ48HZ…
```

## Was es macht

`ttk` sitzt zwischen deinem Coding-Agenten (Claude Code, Codex, …) und der Shell.

1. **Es kompiliert Ausgabe.** Testrunner, Compiler, `git`, Logs und JSON werden
   geparst und auf das Wesentliche reduziert: Status, Zähler, jeder Fehler mit
   Datei und Zeile. Der Exit-Code wird unverändert durchgereicht.
2. **Es verliert nichts.** Das byte-genaue Original landet in einer lokalen
   *Kapsel*. Ein Befehl holt alles, einen Zeilenbereich oder einen Suchtreffer
   zurück. Der Agent muss nie einen teuren Befehl wiederholen, nur um mehr zu sehen.
3. **Es lernt.** Dein Agent markiert die Zeilen, die er nie wieder sehen will, und
   `ttk` macht daraus sichere, nachvollziehbare Filterregeln. Jede Lektion macht
   jeden späteren Lauf kleiner, in diesem Projekt oder mit globalen Filtern in
   allen. Je mehr du es nutzt, desto weniger Rauschen bezahlt dein Agent.
4. **Es führt Buch.** Jeder Lauf in jedem Projekt landet in einem Ledger.
   `ttk gain` zeigt dir, wie viele Tokens du heute und insgesamt gespart hast.

Eine Quality-Firewall prüft jede Kürzung **bevor** sie verwendet wird. Würde ein
Fehler, ein Pfad, eine Zeilennummer oder ein Exit-Code verschwinden, liefert `ttk`
die Originalausgabe. Nichts verlässt deinen Rechner: keine Cloud, kein API-Key,
kein Account.

## ✨ Warum ttk?

- **Bis zu 99 % weniger Tokens** bei Tests, Builds, Logs und `git`
- **Nichts geht verloren.** Die vollständige Ausgabe bleibt lokal, ein Befehl holt sie zurück
- **Fehler bleiben Fehler.** Pfade, Zeilennummern, Exit-Codes und Assertions bleiben wortgleich erhalten
- **Verbessert sich selbst.** Dein Agent markiert den Müll einmal und bezahlt nie wieder dafür
- **Wiederholung kostet nichts.** Beim zweiten gleichen Befehl ist es nur ein Zeiger
- **Secrets bleiben bei dir.** Erkannte Zugangsdaten werden ersetzt, bevor ein Modell sie sieht
- **Läuft offline.** Keine Cloud, kein API-Key, keine Telemetrie, kein Account
- **Ein Binary.** Windows und Linux, keine Laufzeitumgebung
- **Eine Bilanz.** `ttk gain` über alle Projekte statt zwanzig einzeln

## 🔧 Installation

**Windows, Installer (empfohlen):** `ttk-setup-<version>-x86_64-windows.exe` von
der [Release-Seite](https://github.com/Bauvater/ThanosTokenKiller/releases) laden
und doppelklicken. Eine Datei, keine Adminrechte. Der Installer

- installiert `ttk.exe` nach `%LOCALAPPDATA%\Programs\ttk`,
- **ersetzt jede ältere ttk-Version** im `PATH` und setzt diese an die erste Stelle,
- legt den [globalen Ordner](#-der-globale-ordner) `%APPDATA%\ttk` an,
- richtet auf Wunsch Claude Code ein (globale `CLAUDE.md`),
- legt Startmenü-Einträge an und erscheint unter *Einstellungen → Apps*, lässt
  sich also wie jedes Programm wieder entfernen.

Unbeaufsichtigt geht es mit `ttk-setup.exe --yes`.

**Per Skript:**

```bash
# Linux
curl -fsSL https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.ps1 | iex
```

Beide laden das passende Release, **prüfen die SHA-256-Summe** und brechen bei
einer Abweichung ab. Danach führt `ttk setup` durch Workspace, Agent und `PATH`
und fragt vor jedem Schritt.

**Aus dem Quelltext** (Rust 1.90+):

```bash
git clone https://github.com/Bauvater/ThanosTokenKiller.git
cd ThanosTokenKiller
cargo build --release
./target/release/ttk setup
```

Den Windows-Installer selbst bauen: `powershell -File scripts\build-installer.ps1`.
Das Ergebnis liegt in `dist\`.

## ⚡ Loslegen

```bash
ttk                                 # Überblick über das Wichtigste
ttk run -- cargo test               # Befehl ausführen, kompakte Ausgabe erhalten
ttk read --outline src/main.rs      # nur die Deklarationen einer Datei
ttk retrieve <capsule> --level 4    # die vollständige Originalausgabe
ttk suggest                         # Rauschen, das ständig wiederkommt
ttk learn --last                    # dem Filter beibringen, was Müll war
ttk gain                            # gesparte Tokens über ALLE Projekte
ttk global --open                   # den globalen Ordner öffnen
ttk doctor                          # die Installation prüfen
ttk help --all                      # alle Befehle und Optionen
```

## 🧠 Es wird besser, je mehr du es nutzt

Andere Tools liefern eine feste Liste handgeschriebener Filter. `ttk` dreht das
um: **dein Agent schreibt die Filter.** Er markiert, was wertlos war:

```console
$ ttk learn --last <<'EOF'
<filter-trash>
npm WARN deprecated inflight@1.0.6: This module is not supported anymore
npm WARN deprecated glob@7.2.3: This module is not supported anymore
</filter-trash>
added 412 packages in 9s
EOF
```

Ab dann sieht kein Modell diese Zeilen mehr, bei **jedem** `npm install` und mit
jeder Versionsnummer. Das Wissen sammelt sich an drei Stellen:

| Wo | Gilt für | Wie |
|---|---|---|
| `.ttk/learned-filters.json` | dieses Projekt und alle, die es klonen | `ttk learn --last` (Datei committen) |
| `%APPDATA%\ttk\filters\` | **jedes** Projekt auf diesem Rechner | `ttk learn --last --user` oder eine Regeldatei hineinlegen |
| `ttk suggest` | alles, was ständig wiederkommt | schlägt eine Lektion vor, die du prüfst |

Sicher ist das, weil Regeln lesbare Token-Templates statt Regexes sind, weil alles
Unmarkierte als Gegenbeispiel zählt, weil Fehlerzeilen nie gefiltert werden und
weil die Firewall jedes gefilterte Ergebnis prüft. Hat eine Regel zu viel
entfernt, markierst du die Zeile mit `<filter-keep>` und lehrst erneut. Dann wird
die schuldige Regel stillgelegt.

## 📈 Deine Bilanz: `ttk gain`

Jeder `ttk run` in jedem Repository wird in **einem** Ledger gezählt. `ttk gain`
zeigt die Summe, heute, 7 und 30 Tage, ein Tagesdiagramm sowie die Top-Befehle und
-Projekte. `--project` beschränkt die Anzeige auf das aktuelle Projekt, `--days 30`
verlängert das Diagramm, `--json` liefert alles für Skripte.

## 🌍 Der globale Ordner

Alles, was nicht zu einem einzelnen Projekt gehört, liegt unter `%APPDATA%\ttk`
(sonst `~/.config/ttk`). `ttk global --open` öffnet den Ordner.

```text
%APPDATA%\ttk\
├── filters\          globale Filter: JEDE *.json-Regeldatei hier gilt in jedem Projekt
├── usage.jsonl       jede gesparte Token-Zahl aus jedem Projekt (Basis für `ttk gain`)
├── config.toml       optionale Benutzerkonfiguration
└── README.txt        erklärt den Ordner
```

`TTK_GLOBAL_HOME` verlegt den ganzen Ordner, `TTK_USAGE=0` schaltet das Ledger ab.

## 🤖 Agent einrichten

`ttk install` trägt einen markierten Block in `CLAUDE.md` bzw. `AGENTS.md` ein.
Deine eigenen Anweisungen bleiben unangetastet.

```bash
ttk install --target claude --scope global --yes # alle Projekte, über ~/.claude/CLAUDE.md
ttk install --compact                            # ~200-Token-Block statt ~800
```

## 🤝 Beitragen & Lizenz

Pull Requests sind willkommen. Für einen neuen Parser implementierst du
`ttk_compilers::Compiler` und trägst ihn in `registry()` ein. Lizenz: [MIT](LICENSE).
