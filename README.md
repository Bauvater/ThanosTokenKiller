# 🔪 ThanosTokenKiller

> **Dein Coding-Agent liest 3.605 Tokens Testausgabe, um zu erfahren, dass alles grün ist.**
> `ttk` macht daraus 62 – und hebt die Originalausgabe auf, falls doch jemand nachschauen will.

```console
$ ttk run -- cargo test
[test:cargo] status=pass
passed=143 failed=0 ignored=0 duration=6.13s cmd="cargo test" exit=0
raw=cap://cap_01KZ48HZHWBMPN63ETEXQDT001

ttk  ~3605 → ~62 Tokens  ███████████████░  -98%  cap://cap_01KZ48HZ…
```

---

## ✨ Warum ttk?

- **Bis zu 99 % weniger Tokens** bei Tests, Builds, Logs und `git`
- **Nichts geht verloren** – die vollständige Ausgabe bleibt lokal, ein Befehl holt sie zurück
- **Fehler bleiben Fehler** – Dateipfade, Zeilennummern, Exit-Codes und Assertions überleben wortgleich
- **Läuft offline** – keine Cloud, kein API-Key, keine Telemetrie, kein Account
- **Ein einzelnes Binary** – Linux und Windows, keine Laufzeitumgebung
- **Secrets bleiben bei dir** – erkannte Zugangsdaten werden ersetzt, bevor ein Modell sie sieht
- **Der Filter lernt** – dein Agent markiert einmal, was Müll ist, und es kommt nie wieder
- **Wiederholung kostet nichts** – derselbe Befehl zweimal? Beim zweiten Mal nur ein Zeiger
- **Eine Bilanz für alles** – `ttk stats --global` über jedes Projekt, nicht zwanzig einzeln

---

## 🧠 Der Filter, den niemand schreiben muss

Andere Tools liefern eine feste Liste handgeschriebener Filter aus. Irgendwer
merkt, dass `npm install` vierzig Deprecation-Warnungen ausgibt, und irgendwer
schreibt eine Regex. Das skaliert genau so weit wie die Geduld dieser Person.

`ttk` dreht das um: **dein Agent schreibt die Filter.** Er hat die Ausgabe
gerade gelesen und weiß besser als jeder Maintainer, was davon wertlos war. Er
markiert es einfach:

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

ttk 0.1.0 · learned from 2 marked line(s)
  scope            npm install
  rule file        .ttk/learned-filters.json   project

new rules
  id              seen  pattern
  flt_8dfafe90dc     2  npm WARN deprecated {ver} This module is not supported anymore

✓ 1 rule(s) in force for this project
```

Ab jetzt sieht kein Modell diese Zeilen mehr – bei **jedem** `npm install` in
diesem Projekt, mit jeder Versionsnummer.

Und für Ausgabe, die sich *nicht* Zeile für Zeile beschreiben lässt — ein
Banner etwa —, lernt `ttk` eine **Block-Regel** für den ganzen Lauf:

```console
$ ttk rules show flt_cf5c06411c
  kind             block — removes 5 consecutive lines
  pattern          +========================================+
                   | |
                   | ACME TOOLCHAIN, version {n} |
                   | |
                   +========================================+
```

`+====+` ist ein einziges Token: als Zeilen-Regel wäre das „lösche jede Zeile
aus Gleichheitszeichen" — genau die Übergeneralisierung, die die Wächter
verhindern, und sie lehnen sie auch ab. Zusammen sind die fünf Zeilen dagegen
unverwechselbar. Ein Block wird als Ganzes entfernt oder gar nicht, und er ist
**schwerer** zu verdienen als eine Zeilen-Regel, nicht leichter.

Blöcke und Zeilen-Regeln konkurrieren nie: erst werden Zeilen-Regeln gelernt,
und nur ein Lauf, den die *nicht* mehrheitlich beschreiben konnten, wird zum
Block. Vierzig `npm WARN`-Zeilen, die eine Zeilen-Regel schon abdeckt, werden
deshalb kein vierzigzeiliger Block.

**Warum das sicher ist:**

- **Regeln sind Token-Templates, keine Regexes.** Wörter müssen wortgleich
  passen; nur flüchtige Werte (`{n}`, `{ver}`, `{hex}`, `{t}`, `{size}`,
  `{path}`, `{url}`, `{uuid}`) sind Platzhalter. Eine Regel kannst du vorlesen.
- **Was du *nicht* markierst, ist ein Gegenbeispiel.** Eine Regel, die eine
  unmarkierte Zeile treffen würde, wird abgelehnt – mit Begründung.
- **Fehler werden nie gefiltert.** Exit-Codes, Assertions, Stacktraces und
  Zeilennummern erkennt dieselbe Prüfung, die auch die Quality-Firewall nutzt.
  Eine einzige solche Zeile irgendwo im Lauf rettet den ganzen Block.
- **Die Firewall behält das letzte Wort.** Ein gefiltertes Ergebnis wird geprüft
  wie jede andere Kürzung; das Original liegt vorher schon in der Kapsel.
- **Regeln liegen in `.ttk/learned-filters.json`** – lesbares JSON, das du
  committest. Das gesammelte Wissen aller Agenten wandert mit dem Repository.

```bash
ttk rules                     # was wurde gelernt, sortiert nach Ersparnis
ttk rules show <id>           # eine Regel im Detail
ttk rules disable|forget <id> # eine Regel stilllegen oder löschen
ttk filter --explain          # Trockenlauf über stdin, ändert nichts
```

Hat eine Regel doch etwas Nützliches entfernt? Markiere die Zeile mit
`<filter-keep>…</filter-keep>` und lehre erneut – die schuldige Regel wird
stillgelegt. Details in [`docs/learned-filters.md`](docs/learned-filters.md).

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
- 🧠 **Lernende Filter**: `<filter-trash>`-Tags statt handgeschriebener Regexes,
  mit Zeilen- **und** Block-Regeln, `<filter-fold>`-Zusammenfassungen und
  `<filter-only>`-Whitelists
- 🔁 **Wiederholungs- und Delta-Unterdrückung** für Befehle *und* Datei-Reads
- 📂 `ttk read --outline` — die Deklarationen einer Datei statt der Datei
- 🌍 `ttk stats --global` — eine laufende Bilanz über alle Projekte
- 📈 `ttk stats` zeigt, was du bisher gespart hast
- 🎨 Farbige, ausgerichtete Ausgabe – und schlicht, sobald sie in eine Pipe geht
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
- ⬜ Block-Regeln, die sich verallgemeinern (heute: exakter Lauf, exakte Reihenfolge)

---

## 🔧 Installation

**Linux:**

```bash
curl -fsSL https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.ps1 | iex
```

**Windows – Installer (empfohlen):** `ttk-setup-<version>-x86_64-windows.exe`
von der [Release-Seite](https://github.com/Bauvater/ThanosTokenKiller/releases)
laden und doppelklicken. Eine Datei, keine Adminrechte:

- installiert `ttk.exe` nach `%LOCALAPPDATA%\Programs\ttk` und trägt es in
  deinen Benutzer-`PATH` ein,
- legt den **globalen Ordner** `%APPDATA%\ttk` an (globale Filter + Token-Bilanz),
- richtet auf Wunsch Claude Code ein (globale `CLAUDE.md`),
- legt Startmenü-Einträge an und erscheint unter *Einstellungen → Apps*, lässt
  sich also wie jedes Programm wieder entfernen.

Unbeaufsichtigt: `ttk-setup.exe --yes`. Selbst bauen:
`powershell -File scripts\build-installer.ps1` → `dist\ttk-setup-….exe`.

Die Skripte oben laden das passende Release, **prüfen die SHA256-Summe** und weigern sich
bei einer Abweichung. Danach übernimmt `ttk setup`: Workspace, Agent, PATH — und
zum Schluss ein echter Befehl, damit du die Ersparnis auf deiner eigenen Ausgabe
siehst, statt sie hier zu lesen. Jeder Schritt fragt vorher.

Nichts landet systemweit, nichts braucht Adminrechte.

**Aus dem Quelltext**, wenn du [Rust](https://rustup.rs) (1.90+) hast:

```bash
git clone https://github.com/Bauvater/ThanosTokenKiller.git
cd ThanosTokenKiller
cargo build --release
./target/release/ttk setup
```

---

## ⚡ Loslegen

```bash
ttk run -- cargo test               # Befehl ausführen, kompakte Ausgabe erhalten
ttk read --outline src/main.rs      # nur die Deklarationen einer Datei
ttk suggest                         # was ständig wiederkommt und niemand braucht
ttk learn --last                    # markierten Müll für immer loswerden
ttk rules                           # was der Filter gelernt hat
ttk gain                            # was hat es über ALLE Projekte gespart?
ttk global --open                   # der globale Ordner mit den globalen Filtern
ttk retrieve <capsule> --level 4    # die vollständige Originalausgabe
ttk help                            # Überblick; ttk help --all für alle Befehle
```

Denselben Befehl zweimal auszuführen kostet nicht zweimal: identische Ausgabe
schrumpft auf `[repeat] identical to cap://…`, fast identische auf ein `[delta]`
des Unterschieds. Fehlerzeilen kommen in beiden Fällen mit — ein wiederholter
*Fehlschlag* zeigt weiterhin den Fehler.

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
über `ttk run --` laufen sollen, wie die kompakte Ausgabe zu lesen ist, wie
der Agent sich die vollständige Ausgabe zurückholt statt einen teuren Befehl
noch einmal auszuführen – und wie er dem Filter beibringt, welche Zeilen er nie
wieder sehen will.

Deine eigenen Regeln bleiben unangetastet – der Block steht zwischen Markern und
wird beim nächsten Mal an Ort und Stelle aktualisiert.

Für Skripte:

```bash
ttk install --target all --scope project --yes
ttk install --target all --yes --no-path     # ohne PATH anzufassen
```

---

## 📈 Deine Bilanz — über alle Projekte

Jeder `ttk run`, in jedem Repository, landet in **einer** Datei außerhalb aller
Projekte. Nur so kann die Frage „wie viel hat mir das gebracht" beantwortet
werden, ohne zwanzig Workspaces abzuklappern – mit einem Befehl, von überall:

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
  7 projects · since 2026-06-02

▌ last 14 days  ▂▅▃█▁▁▄▆▃▇▂▁▅▆
  today      ███████████████░░░░░░░░░░░░░      ~41 877  23 runs
  Thu 09-24  ████████████████████░░░░░░░░      ~55 012  31 runs
  …

▌ top commands
  command     saved                    rate  runs
  cargo    ~702 118  ████████████████   73%   188
  pytest   ~381 044  █████████░░░░░░░   69%    96

▌ global folder
  folder           C:\Users\you\AppData\Roaming\ttk
  global filters   12 rules in 2 files  filters\
  savings ledger   usage.jsonl  every run in every project, local only
```

`ttk gain --project` zeigt nur das aktuelle Projekt, `ttk gain --days 30`
verlängert das Diagramm, `ttk gain --json` ist für Skripte.

### Der globale Ordner

Alles, was nicht zu einem einzelnen Projekt gehört, liegt an **einer** Stelle
(`%APPDATA%\ttk` unter Windows, `~/.config/ttk` sonst – `ttk global` zeigt es,
`ttk global --open` öffnet ihn):

```text
%APPDATA%\ttk\
  filters\             globale Filter: JEDE *.json-Regeldatei hier gilt in jedem Projekt
    learned-filters.json   was `ttk learn --user` lernt
  usage.jsonl          jede gesparte Token-Zahl, aus jedem Projekt – die Basis für `ttk gain`
  config.toml          optionale Benutzerkonfiguration
```

Regeln aus `ttk rules export` lassen sich einfach als Datei in `filters\`
legen. Der Ordner enthält lokale Pfade und verlässt die Maschine nie.
`TTK_USAGE=0` schaltet das Ledger ab, `ttk usage forget --all` leert es,
`TTK_GLOBAL_HOME` verlegt den ganzen Ordner.

<sub>Beispielausgabe – zeigt das Format, nicht deine Zahlen.</sub>

Für dieses eine Projekt:

```console
$ ttk stats
ttk 0.1.0 · lifetime statistics

      ~1 284 933  ████████████████████░░░░░░░░  71.4% of ~1 799 204 tokens
  tokens that never reached a model

  context sent         ~514 271
  commands run              412
  sessions                   36

where the savings came from
  command   saved      runs
  cargo     ~702 118   ████████████████  188 runs
  pytest    ~381 044   ████████░░░░░░░░   96 runs
  git        ~14 119   ░░░░░░░░░░░░░░░░   74 runs
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

ttk  ~3605 → ~62 tokens  ███████████████░  -98%  cap://cap_01KZ48HZ…
```

---

## ✨ Why ttk?

- **Up to 99% fewer tokens** on tests, builds, logs and `git`
- **Nothing is lost** – the full output stays on disk, one command brings it back
- **Errors stay errors** – file paths, line numbers, exit codes and assertions survive word for word
- **Runs offline** – no cloud, no API key, no telemetry, no account
- **A single binary** – Linux and Windows, no runtime to install
- **Secrets stay yours** – detected credentials are replaced before a model sees them
- **The filter learns** – your agent marks the junk once and never sees it again
- **Repetition is free** – run the same command twice and the second one is a pointer
- **One balance sheet** – `ttk stats --global` across every project, not twenty separately

---

## 🧠 The filter nobody has to write

Every other tool in this space ships a fixed list of hand written filters.
Somebody notices that `npm install` prints forty deprecation warnings, and
somebody writes a regex. That scales exactly as far as that person's patience.

`ttk` inverts it: **your agent writes the filters.** It has just read the output
and knows better than any maintainer which parts of it were worthless, so it
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

ttk 0.1.0 · learned from 2 marked line(s)
  scope            npm install
  rule file        .ttk/learned-filters.json   project

new rules
  id              seen  pattern
  flt_8dfafe90dc     2  npm WARN deprecated {ver} This module is not supported anymore

✓ 1 rule(s) in force for this project
```

From now on no model sees those lines again — on **every** `npm install` in this
project, at any version number.

And for output that *cannot* be described line by line — a banner, say — `ttk`
learns a **block rule** for the whole run:

```console
$ ttk rules show flt_cf5c06411c
  kind             block — removes 5 consecutive lines
  pattern          +========================================+
                   | |
                   | ACME TOOLCHAIN, version {n} |
                   | |
                   +========================================+
```

`+====+` is a single token: as a line rule that would mean "delete any line made
of equals signs" — exactly the over-generalisation the guards exist to prevent,
and they duly refuse it. Together those five lines are unmistakable. A block is
removed as a whole or not at all, and it is **harder** to earn than a line rule,
not easier.

Blocks and line rules never compete: line rules are learned first, and only a
run they could not describe for most of its lines becomes a block. Forty
`npm WARN` lines that one line rule already covers do not also turn into a forty
line block.

**Why that is safe:**

- **Rules are token templates, not regexes.** Words have to match exactly; only
  volatile values (`{n}`, `{ver}`, `{hex}`, `{t}`, `{size}`, `{path}`, `{url}`,
  `{uuid}`) are wildcards. You can read a rule out loud.
- **Whatever you do *not* mark is a counter-example.** A rule that would also
  match an unmarked line is refused, with a reason.
- **Errors are never filtered.** Exit codes, assertions, stack traces and line
  references are recognised by the same pass the quality firewall uses. One such
  line anywhere in a run saves the entire block.
- **The firewall keeps the last word.** A filtered result is reviewed like any
  other shortening, and the original is already in a capsule before it runs.
- **Rules live in `.ttk/learned-filters.json`** — readable JSON you commit. The
  accumulated knowledge of every agent travels with the repository.

```bash
ttk rules                     # what has been learned, ranked by tokens saved
ttk rules show <id>           # one rule in detail
ttk rules disable|forget <id> # retire or delete a rule
ttk filter --explain          # dry run over stdin, changes nothing
```

A rule removed something useful after all? Mark that line with
`<filter-keep>…</filter-keep>` and teach again — the offending rule retires.
The whole design is in [`docs/learned-filters.md`](docs/learned-filters.md).

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
- 🧠 **Learned filters**: `<filter-trash>` tags instead of hand written regexes,
  with line **and** block rules, `<filter-fold>` summaries and `<filter-only>`
  whitelists
- 🔁 **Repeat and delta suppression** for commands *and* file reads
- 📂 `ttk read --outline` — a file's declarations instead of the file
- 🌍 `ttk stats --global` — one running total across every project
- 📈 `ttk stats` shows what you have saved so far
- 🎨 Coloured, aligned output — and plain the moment it goes into a pipe
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
- ⬜ Block rules that generalise (today: an exact run, in an exact order)

---

## 🔧 Installation

**Linux:**

```bash
curl -fsSL https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.ps1 | iex
```

**Windows installer (recommended):** download
`ttk-setup-<version>-x86_64-windows.exe` from the
[releases page](https://github.com/Bauvater/ThanosTokenKiller/releases) and
double-click it. One file, no administrator rights. It installs `ttk.exe` to
`%LOCALAPPDATA%\Programs\ttk`, adds it to your user `PATH`, creates the
**global folder** `%APPDATA%\ttk` (global filters and the savings ledger),
optionally sets up Claude Code, and registers itself under *Settings → Apps* so
it uninstalls like any other program. Unattended: `ttk-setup.exe --yes`. Build
it yourself with `powershell -File scripts\build-installer.ps1`.

The scripts above fetch the release for your platform, **verify its SHA256** against the
published checksum, and refuse to continue if they disagree. Then `ttk setup`
takes over: workspace, agent, `PATH` — and finally a real command, so the first
thing you see is the saving on your own output rather than a claim in a README.
Every step asks first.

Nothing is installed system-wide and nothing needs an elevated prompt.

**From source**, if you have [Rust](https://rustup.rs) (1.90+):

```bash
git clone https://github.com/Bauvater/ThanosTokenKiller.git
cd ThanosTokenKiller
cargo build --release
./target/release/ttk setup
```

---

## ⚡ Getting started

```bash
ttk run -- cargo test               # run a command, get compact output
ttk read --outline src/main.rs      # a file's declarations instead of the file
ttk suggest                         # what keeps coming back that nobody needs
ttk learn --last                    # get rid of marked junk for good
ttk rules                           # what the filter has learned
ttk gain                            # what has it saved across ALL projects?
ttk global --open                   # the global folder with your global filters
ttk retrieve <capsule> --level 4    # the complete original output
ttk help                            # overview; ttk help --all for everything
```

Running the same command twice does not cost twice: identical output collapses
to `[repeat] identical to cap://…`, mostly-identical output to a `[delta]` of
what changed. Error lines travel with both, so a repeated *failure* still shows
the failure.

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

Every `ttk run`, in every repository, is counted in **one** ledger outside all
projects, so one command answers "how much has this saved me" from anywhere:

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
```

It goes on with a daily chart, the top commands and projects, and where the
global folder is. `ttk gain --project` narrows it to the current project,
`--days 30` lengthens the chart, `--json` is for scripts.

### The global folder

Everything that is not tied to one project lives in one place (`%APPDATA%\ttk`
on Windows, `~/.config/ttk` elsewhere; `ttk global --open` opens it):

```text
filters\          global filters: EVERY *.json rule file here applies in every project
usage.jsonl       every token saved, from every project — what `ttk gain` adds up
config.toml       optional user configuration
```

`ttk learn --user` writes to `filters\learned-filters.json`; a file from
`ttk rules export` can simply be dropped in. `TTK_GLOBAL_HOME` moves the whole
folder, `TTK_USAGE=0` switches the ledger off.

<sub>Example output – shows the format, not your numbers.</sub>

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
