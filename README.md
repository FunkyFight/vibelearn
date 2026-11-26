# vibelearn

An AI pair that reads your code each time you save, finishes the obvious leftovers for you and tells you what will break or rot, without taking the keyboard away from you. When you want to learn something new, the same tool becomes a tutor that guides you until you have built it yourself.

```powershell
vibelearn assist
```

VS Code opens with a panel next to your code. You code, you save, and a few seconds later vibelearn has gone through your changes.

## Contents

- [Assist mode](#assist-mode)
- [Requirements](#requirements)
- [Installation](#installation)
- [The other modes](#the-other-modes)
- [In VS Code](#in-vs-code)
- [Talking to vibelearn from your code](#talking-to-vibelearn-from-your-code)
- [Command line options](#command-line-options)
- [Settings](#settings)
- [Connecting a model](#connecting-a-model)
- [Updates](#updates)
- [Profile and reviews](#profile-and-reviews)
- [Where your data lives](#where-your-data-lives)
- [Environment variables](#environment-variables)
- [How it works](#how-it-works)
- [Development](#development)
- [Troubleshooting](#troubleshooting)

## Assist mode

Assist mode is the one to run all day. You stay the author of your code: vibelearn only does the part that requires no decision, and talks to you about the rest.

```powershell
vibelearn assist "the refactor of BeatPattern"
```

The description is optional. It only tells vibelearn what you are working on.

### It finishes what you started

When you begin a change and leave places that still use the old form, those places are finished for you:

- a name you renamed that still has its old spelling elsewhere;
- a value you turned into a constant or a field, still written as a literal elsewhere with the same meaning;
- a type you just introduced, while other code still builds or reads the old form (a tuple, loose variables, parallel fields);
- a method you just extracted, while copies of the same code remain elsewhere: the copies become calls to it;
- a call that still uses a signature you changed, when adapting it is mechanical;
- a typo that breaks the build and has a single possible correction (a missing `;`, a misspelled existing name).

An edit is made only when all of this holds:

- it follows from something you already wrote, and vibelearn can point at the line;
- anyone reading your diff would make the same edit: one right answer, nothing to weigh;
- the program does exactly what your own change already decided, no more;
- it is short, 8 lines at most per place;
- it is not the code you are typing right now.

### It leaves every decision to you

vibelearn never creates a class, a method, a field or a name you did not create. It never chooses between two ways of doing something, picks a value, moves code, deletes code, fixes a logic bug or starts a restructuring on its own. Those are technical decisions: it tells you about them in a remark and you decide.

### Every edit is visible and undoable

Each edit is shown on its line in the editor with its reason ("renamed on line 4"), and listed in the panel under "On your code" with an "Undo" link. Ctrl+Z works too. If you undo an edit, vibelearn does not make it again and takes it as information about what you meant.

### It tells you what will break or rot

At each save, vibelearn annotates the lines you wrote, four remarks at most, the most important first:

| Remark | What it is |
| --- | --- |
| To fix | A bug, a wrong unit or sign, an edge case that fails, a compile error explained, a misused API. |
| Design | The code works against lasting structure: it names the class to create or the pattern to use, and what it saves you later. It says so at the first save where it shows, not eight features later. |
| Good to know | A fact you want on that line: a default, a unit, a pitfall of the API you just used. |
| Good | A choice worth confirming. Rare. |

A click on a remark takes you to its line. On hover, two links:

- **Teach me** starts a short lesson on the notion behind the remark, right there in your code. During that lesson you write every line of the piece yourself.
- **Forget it** removes the remark, and vibelearn stops making that kind of remark, in this session and the following ones.

When there is nothing worth saying, it says nothing.

### It answers in full

In assist mode a question gets a direct and complete answer, solution included. Type it in the panel (it is about the line under your cursor) or write it in your code as a comment: `//? why this cast?`. See [Talking to vibelearn from your code](#talking-to-vibelearn-from-your-code) for the pseudo-code, next step and short lesson comments.

### When your changes are sent

Your changes leave after 6 seconds without a modification. `--auto-delay 3` or `vibelearn config auto-delay 3` changes that. With `--no-auto`, nothing is sent until you click "Check".

`vibelearn monitor` is the same mode without the edits: remarks and answers only.

## Requirements

- **VS Code** with the `code` command in your `PATH`. An editor of the same family (Cursor, VSCodium, Windsurf) is chosen with `--editor`.
- **A model**, one of:
  - your Claude subscription, through the native Claude Code binary (`winget install Anthropic.ClaudeCode`). No API key, no Node;
  - an API key: the official Claude API, an Anthropic-compatible service, an OpenAI-compatible service (Ollama, LM Studio and others) or the OpenAI Responses API.
- **Rust** 1.88 or later, only to build from source (edition 2024).

Developed and tested on Windows.

## Installation

Download the executable from the [latest release](https://github.com/FunkyFight/vibelearn/releases/latest) and put it in a folder of your `PATH`. It then keeps itself up to date (see [Updates](#updates)).

Or build it from source:

```powershell
cargo install --git https://github.com/FunkyFight/vibelearn
```

Then check the setup:

```powershell
vibelearn doctor
```

`doctor` checks the connection to the model, the `claude` binary and where your profile is. If the subscription is not signed in:

```powershell
vibelearn login
```

The VS Code extension is embedded in the executable. vibelearn installs it at the first launch and reinstalls it when its version changes. There is nothing to install by hand.

## The other modes

| Command | What it does |
| --- | --- |
| `vibelearn assist [about]` | Reviews each save and finishes the obvious leftovers itself. See [Assist mode](#assist-mode). |
| `vibelearn monitor [about]` | Reviews each save without editing anything. |
| `vibelearn "<goal>"` | Learn by building: a plan of steps, from the first notion to the finished feature. You write all of it. |
| `vibelearn refactor [target]` | Restructure existing code (SOLID, DRY, extensible) without changing what it does. You are the one rewriting it. No target: the whole project. |
| `vibelearn review` | Review the notions that are due (spaced repetition). |
| `vibelearn resume` | Resume the session in progress in this project. |
| `vibelearn profile` | Show your profile: notions, levels, upcoming reviews. |
| `vibelearn config [setting [value]]` | List, read or change a setting. |
| `vibelearn login [provider]` | Choose how to reach the model. |
| `vibelearn logout` | Forget the saved API key, or sign out of the subscription. |
| `vibelearn update` | Install the latest release now. |
| `vibelearn doctor` | Check the installation. |

### Learning sessions

```powershell
vibelearn "add a wall jump to my player"
```

1. vibelearn reads your project (stack, file tree, recent git history) and opens VS Code.
2. It asks once what you already know, with a list of notions to set to "I know it", "Unsure" or "To learn".
3. It cuts the work into steps. For each one the panel shows what you need to know and what you have to write, and the line where to write it is highlighted in the editor.
4. You code and save. vibelearn annotates your code and validates the step or tells you what is missing.
5. At the end it writes a revision sheet in `.vibelearn/notes/`.

Three rules hold in learning and refactor sessions. They are written in the tutor's prompt ([src/system_prompt.md](src/system_prompt.md)).

1. **It never writes the code of your task.** Not the implementation, not the exact edit to make, not a line by line recipe. When you are stuck, it makes the next step smaller. This holds even if you ask for it.
2. **It states the facts before asking you anything.** The order of a function's parameters, the unit of a value, a classic pitfall: those cannot be worked out, so it gives them up front. It only leaves you what can be deduced from what you have.
3. **The code must last.** Working is half of done. It makes you structure: one class per responsibility, no magic value copied around, rules stored in data instead of `if` chains. When needed, the next step is the restructuring, before the feature.

In a learning session, outside the step you are working on, it still finishes your mechanical leftovers the way assist mode does. Not in a refactor session: there, finishing the refactoring is the exercise.

You can close the terminal at any time. `vibelearn resume`, or `vibelearn` alone, picks up where you stopped. Running `vibelearn` with the same topic and mode resumes the session; another topic moves the old one to `.vibelearn/history/` and starts a new one. `assist` and `monitor` do not touch a learning session in progress.

## In VS Code

### The panel

It has three fixed areas.

**At the top, the state.** The mode or the current step ("Step 2 of 5"), what vibelearn is doing, then three buttons:

- **Check** sends your code now, without waiting for the delay;
- **`//`** replaces the conversation with the list of comments you can write in your code (Escape or a second click to come back);
- **⋯** holds the rest: "Where are we?", "Skip this step", "Wrap up the session", "Stop the session". The model and the effort in use are shown there.

**In the middle, what you read.** Only this area scrolls.

- "On your code": the edits made by vibelearn, each with "Undo".
- "Conversation": your questions and its answers. The latest one is shown in full, the earlier ones are folded.
- In a lesson: the title of the step, "Good to know", then "Your turn" with the success criteria and a "Where to write?" link.
- When vibelearn asks you something (a choice, a free answer, a list of notions, an approval before writing a file or running a command), it comes before everything else.

**At the bottom, the field.** Enter sends, Shift+Enter starts a new line, and the field grows with your text. Your question is about the line under your cursor in the editor, shown under the field. In a lesson, the helpers sit just above: "Hint", "I'm stuck", "Explain differently", "Go deeper".

A single word in the field is the same as the matching button: `hint`, `check`, `stuck`, `explain`, `deeper`, `recap`, `skip`, `finish`, `plan`, `notes`, `help`, `quit`. `/run <command>` runs a command in your project and shows its output to vibelearn.

### The floating window

The remarks on your last save, sorted by severity, and the analysis vibelearn writes when it starts and after each save, are not in the conversation: they sit in a small window of their own, opened by vibelearn itself. It has no title bar and stays on top of every other window, so you can put it anywhere, outside VS Code too.

- Drag it by its top strip. Resize it from its bottom right corner. Its place and size are kept in `~/.vibelearn/float.json`.
- The arrow on the right of the strip, or a double click on the strip, folds it down to the strip alone (the number of remarks stays visible) and unfolds it.
- A click on a remark goes to its line. "Detail" unfolds the explanation; "Teach me" and "Forget it" show on hover.

In assist and monitor mode it opens with the session; in a learning session, at the first remark. `VIBELEARN_NO_FLOAT=1` turns it off.

### In the editor

- Edits made by vibelearn are marked on their line, with the reason and an undo link on hover.
- Remarks are shown at the end of the line they are about. The detail is on hover.
- The pseudo-code you asked for is shown as a block next to your code.
- In a lesson, the line where to write is highlighted.
- The status bar at the bottom says what vibelearn is doing. A click reopens the panel.
- Right click on a line: "vibelearn: Ask about this line".

Commands in the palette (Ctrl+Shift+P): "vibelearn: Open the panel", "Ask about this line", "Give me a hint", "Check my code".

The interface is in English or in French, following your system (`--lang` forces one). The tutor answers in the language you write to it.

## Talking to vibelearn from your code

Write one of these comments on its own line, then save.

| Comment | What you get | Once read |
| --- | --- | --- |
| `//? why this cast?` | An answer to your question about this line, in the panel. | removed from the file |
| `//n wall jump` | The next step of that feature, at the end of the line. | removed from the file |
| `//t coroutines` | A short lesson, right there in your code. | removed from the file |
| `//// make the player jump` | The pseudo-code in sentences, as a block next to your code. | stays in the file |
| `////! make the player jump` | The formal pseudo-code (assignments, `if`, `for each`, `return`). | stays in the file |

A space must follow `n` and `t`: `//todo` stays an ordinary comment.

The pseudo-code block lists first what has to be created (the fields, marked `+`), then the numbered steps, with details indented under them. A title appears only when the code belongs somewhere other than where you wrote the request. "Clear", on hover, removes the block.

The mark follows the comment opener of your language:

| Comment opener | Languages | Question | Pseudo-code |
| --- | --- | --- | --- |
| `//` | Rust, C, C++, C#, Java, Kotlin, Swift, Go, JavaScript, TypeScript, Dart, PHP, Zig, shaders and others | `//?` | `////` |
| `#` | Python, Ruby, shell, PowerShell, GDScript, TOML, YAML, Elixir, Julia and others | `#?` | `####` |
| `--` | Lua, SQL, Haskell, Elm, Ada | `--?` | `----` |
| `;` | Lisp, Clojure, Scheme, assembly | `;?` | `;;;;` |
| `'` | Visual Basic | `'?` | `''''` |
| `/* */` | CSS | `/*? … */` | not available |
| `<!-- -->` | HTML, XML, Vue, Svelte | `<!--? … -->` | not available |

Apart from removing those comments and making the assist edits, vibelearn changes nothing in your files: line endings, BOM and indentation stay as they are.

## Command line options

They apply to every session mode.

| Option | Effect |
| --- | --- |
| `--new` | Start from scratch even if a session on this topic exists. |
| `--model <name>` | Model to use. With Claude: `opus`, `sonnet`, `haiku` or a full name. |
| `--effort <level>` | Thinking effort: `low`, `medium`, `high`, `xhigh`, `max`. |
| `--auto-delay <seconds>` | Seconds without a modification before your changes are sent. |
| `--no-auto` | Only send your changes when you click "Check". |
| `--editor <command>` | Editor command (default: `code`). `none`: neither opens the editor nor installs the extension. |
| `--claude-bin <path>` | Path of the `claude` binary, when it is not found on its own. |
| `--use-api-key` | With the `claude` binary, use `ANTHROPIC_API_KEY` instead of the subscription. |
| `--lang fr` or `--lang en` | Interface language (default: the system's). |

An option always wins over the saved setting.

## Settings

```powershell
vibelearn config
```

| Setting | Default | Values |
| --- | --- | --- |
| `auto-delay` | 6 | 1 to 600 seconds |
| `model` | your subscription's | `opus`, `sonnet`, `haiku` or a full name |
| `effort` | `medium` | `low`, `medium`, `high`, `xhigh`, `max` |
| `auto-update` | `on` | `on`, `off` |

```powershell
vibelearn config auto-delay 3
```

`vibelearn config <setting> default` goes back to the default. Settings are stored in `~/.vibelearn/config.json`.

## Connecting a model

`vibelearn login` with no argument opens a menu. You can also name the provider:

| Provider | What it is |
| --- | --- |
| `claude` | Your Claude subscription, through the `claude` binary. No key. This is the default. |
| `claude-api` | The official Claude API, with an Anthropic API key. |
| `anthropic` | A service compatible with the Anthropic Messages API (URL and key). |
| `openai` | A service compatible with the OpenAI Chat Completions API (URL and key). |
| `codex` | The OpenAI Responses API, the one used by Codex models. |

`--url` and `--model` skip the questions. Example with a local model served by Ollama:

```powershell
vibelearn login openai --url http://localhost:11434/v1 --model qwen3
```

The key is typed without being displayed. It can also come from the `VIBELEARN_API_KEY` variable. vibelearn checks it with the service before saving it in `~/.vibelearn/login.json`.

With a key there is no binary: vibelearn holds the conversation itself and runs the tools. Reading and searching your project are free; writing a file or running a command goes through an approval in the panel.

## Updates

vibelearn updates itself from the [releases of this repository](https://github.com/FunkyFight/vibelearn/releases).

- **During a session**, it looks at the latest release in the background. If it is newer than the version running, it downloads it and puts it in place of the executable. A line in the terminal says so, and the new version is used at the next launch. That next session tells you in the panel which release you are running.
- **On demand**, `vibelearn update` does the same right away and says what it did. `vibelearn update --force` reinstalls the latest release even when it is not newer.
- **To turn it off**: `vibelearn config auto-update off`. `vibelearn update` still works.

```powershell
vibelearn update
```

How it decides: the tag of the latest release (`v0.2.0`) is compared with the version of the executable. Drafts and pre-releases are ignored, and a tag that is not a version number never replaces anything. A tag is installed once, never downloaded twice. A debug build never updates itself in the background.

A running executable cannot be overwritten on Windows, so the old one is renamed to `vibelearn.exe.old` and removed at a later launch. What the updater remembers is in `~/.vibelearn/update.json`.

## Profile and reviews

vibelearn remembers what you know from one project to the next. Each notion has a level:

| Level | Name |
| --- | --- |
| 0 | unknown |
| 1 | seen |
| 2 | understood |
| 3 | applied |
| 4 | mastered |

A notion you tick in the starting list is recorded as self-reported. It only counts as demonstrated once vibelearn has seen it in your code or in your answers. Each notion has a review date, which moves further away as you master it.

```powershell
vibelearn profile
```

```powershell
vibelearn review
```

`profile` shows your notions, your sessions, your day streak and what is due. `review` starts a session on the notions that are due.

## Where your data lives

For you, in `~/.vibelearn/` (or the folder given by `VIBELEARN_HOME`):

| File | Content |
| --- | --- |
| `profile.json` | your learner profile |
| `config.json` | your settings |
| `login.json` | the chosen connection: provider, URL, key, model |
| `update.json` | the last release installed by the updater |
| `float.json` | where the floating window sits and its size |

In each project, in `.vibelearn/`:

| Path | Content |
| --- | --- |
| `session.json` | the session in progress, for `vibelearn resume` |
| `history/` | earlier sessions |
| `notes/` | your revision sheets |
| `demos/` | the small examples written by the tutor |
| `conversations/` | the conversations held by vibelearn when it goes through an API |
| `ui/` | the exchange with the VS Code extension |

This folder ignores itself in git (it contains a `.gitignore` with `*`). vibelearn never touches your project's `.gitignore`.

## Environment variables

| Variable | Role |
| --- | --- |
| `VIBELEARN_HOME` | Replaces `~/.vibelearn`. |
| `VIBELEARN_CLAUDE` | Path of the `claude` binary (same as `--claude-bin`). |
| `VIBELEARN_EDITOR` | Editor command (same as `--editor`). |
| `VIBELEARN_NO_FLOAT` | Set to anything: the floating window of remarks does not open. |
| `VIBELEARN_API_KEY` | API key read by `vibelearn login`, instead of typing it. |
| `ANTHROPIC_API_KEY` | Used only with `--use-api-key`. Otherwise it is removed from the environment of the `claude` binary, so that the subscription is used. |

## How it works

```
vibelearn (terminal)                          extension (VS Code)

   writes  .vibelearn/ui/state.json     --->   shows the panel and the markers
   reads   .vibelearn/ui/inbox/*.json   <---   drops answers, questions, commands

   watches your files, computes the diffs since the last send
        |
        v
   engine: `claude` binary (subscription) or API (key)
        |
        v
   tutor tools: remarks, assist edits, list of notions, plan, steps,
                verdict, pseudo-code, revision sheet
```

- **The terminal** only starts and stops the session. Everything else happens in VS Code.
- **The exchange with the extension** goes through files in `.vibelearn/ui/`. vibelearn writes `state.json` (everything there is to show). The extension drops its messages in `inbox/` and confirms in `ack.json` the last version it displayed.
- **The watcher** keeps a reference copy of your text files and sends the tutor the diff of what you changed. It respects your `.gitignore`.
- **The engine** is interchangeable. With the subscription, vibelearn drives the `claude` binary through the Claude Agent SDK protocol (JSON lines on stdin and stdout) and exposes its tools through an in-memory MCP server named `tutor`. With a key, it talks to the API directly and runs the file tools itself. The application does not see the difference.
- **The tutor** acts through its tools, not through free text. That is what lets a remark, an edit, a checklist or a verdict be shown as real interface elements.
- **An assist edit** is not written to disk by vibelearn: the extension applies it in the editor, which is why Ctrl+Z undoes it.

### Modules

| File | Role |
| --- | --- |
| [src/main.rs](src/main.rs) | command line, choice of the mode and of the engine |
| [src/app.rs](src/app.rs) | session controller: events from the engine, the files and the developer |
| [src/front.rs](src/front.rs) | publishes the state to VS Code, reads what the extension sends back, interface texts |
| [src/threads.rs](src/threads.rs) | what is shown outside the code: line where to write, remarks, pseudo-code, conversation |
| [src/assist.rs](src/assist.rs) | the small edits of assist mode |
| [src/annotate.rs](src/annotate.rs) | reads the `//?`, `//n`, `//t`, `////` comments and touches files without altering anything else |
| [src/watch.rs](src/watch.rs) | file watching and diffs |
| [src/claude.rs](src/claude.rs) | engine through the `claude` binary |
| [src/api.rs](src/api.rs) | engine through an API: Anthropic Messages, OpenAI Chat Completions, OpenAI Responses |
| [src/localtools.rs](src/localtools.rs) | file and command tools run by vibelearn when it goes through an API |
| [src/mock.rs](src/mock.rs) | scripted tutor, for the tests and for trying the interface offline |
| [src/tools.rs](src/tools.rs) | definition of the tutor's tools |
| [src/prompt.rs](src/prompt.rs), [src/system_prompt.md](src/system_prompt.md) | everything that is said to the model |
| [src/modal.rs](src/modal.rs) | state of the questions, checklists and approvals |
| [src/session.rs](src/session.rs) | session state, saved in `.vibelearn/session.json` |
| [src/profile.rs](src/profile.rs) | learner profile and spaced repetition |
| [src/project.rs](src/project.rs) | quick survey of the project at startup |
| [src/runner.rs](src/runner.rs) | commands run for `/run` and for the automatic check of a step |
| [src/update.rs](src/update.rs) | self-update from the GitHub releases |
| [src/login.rs](src/login.rs), [src/config.rs](src/config.rs), [src/paths.rs](src/paths.rs) | connection, settings, locations |
| [src/i18n.rs](src/i18n.rs) | interface language (French, English) |
| [src/vscode.rs](src/vscode.rs) | the `code` command, building and installing the `.vsix` package |
| [src/float.rs](src/float.rs) | the floating window of remarks (egui), on its own thread |
| [src/gui.rs](src/gui.rs) | optional window (egui), only built with `--features window` |
| [vscode/extension.js](vscode/extension.js) | the VS Code extension: panel, decorations, hovers, status bar |

## Development

```powershell
cargo test
```

The tests play a whole session against the scripted tutor, from the checklist to the revision sheet, with no model and no window. The updater is tested against a local stand-in for GitHub, including the replacement of an executable while it runs.

To try the interface without a model:

```powershell
vibelearn assist --mock
```

**Changing the extension.** It lives in [vscode/extension.js](vscode/extension.js) and [vscode/package.json](vscode/package.json), embedded in the executable at build time. Its version is a fingerprint of those files: after a `cargo install`, vibelearn sees that it changed and reinstalls it at the next launch. The extension then offers to reload the VS Code window.

**Adding an interface text.** All the panel texts come from `strings()` in [src/front.rs](src/front.rs), in French and in English. The extension only holds fallbacks.

**Adding a setting.** A field in `Config` and an entry in `SETTINGS`, in [src/config.rs](src/config.rs).

**Publishing a release.**

1. Raise `version` in [Cargo.toml](Cargo.toml).
2. Run [build.cmd](build.cmd). It builds the Windows executable, cross-compiles the Linux one with [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), and puts both in `dist\` under the names below.
3. Create a GitHub release tagged `v<version>` and attach the files of `dist\` as they are, not in an archive.

`build.cmd` needs cargo-zigbuild and zig, once:

```powershell
cargo install cargo-zigbuild
```

```powershell
winget install zig.zig
```

The updater picks the asset from its name:

| System | Asset name |
| --- | --- |
| Windows | any `.exe`: `vibelearn.exe`, `vibelearn-windows-x86_64.exe` |
| Linux | a name containing `linux`: `vibelearn-linux-x86_64` |
| macOS | a name containing `macos` or `darwin`: `vibelearn-macos-arm64` |

When several assets fit, the one naming the machine's architecture (`x86_64`, `amd64`, `x64`, `aarch64`, `arm64`) wins. The tag must match the version in `Cargo.toml`: installed copies compare the two to decide whether to update.

**The optional window.** An egui interface exists as an alternative to VS Code. It is not built by default:

```powershell
cargo install --path . --features window
```

It is then started with `--window`.

[design/maquette.html](design/maquette.html) is the reference mock of the panel and of the pseudo-code block.

## Troubleshooting

**"The `claude` binary was not found".** Install Claude Code (`winget install Anthropic.ClaudeCode`), or give its path with `--claude-bin` or `VIBELEARN_CLAUDE`. `vibelearn doctor` says what it finds.

**The panel does not appear, or it has the old look.** Reload the VS Code window. You can also reopen it from the palette: "vibelearn: Open the panel".

**VS Code does not open.** The `code` command is not in your `PATH`. Add it, or give the right command with `--editor`.

**Nothing is sent when I save.** Look at the state at the top of the panel. With `--no-auto` you have to click "Check". If file watching could not start, vibelearn says so at launch and "Check" stays available.

**vibelearn made an edit I did not want.** Click "Undo" on it in the panel, or press Ctrl+Z. It will not make that edit again. If you want no edits at all, use `vibelearn monitor`.

**"Access denied" during `cargo install` (Windows).** vibelearn is still running and locks its executable. Stop the session, then install again. `vibelearn update` does not have this problem.

**The update does nothing.** `vibelearn update` says why: no release published, a release that is not newer than your version, or a release with no executable for your system.

**The API key is refused.** `vibelearn doctor` checks it again with the service. `vibelearn login` lets you give another one.
