// vibelearn pour VS Code.
// - Un panneau à côté du code : l'étape en cours (à savoir, à toi), les retours,
//   les questions et réponses, la checklist et les questions à choix, les boutons.
// - Dans l'éditeur : la ligne où écrire surlignée, et des annotations courtes en
//   bout de ligne sur ce que l'apprenant a écrit (le détail au survol).
// Rien n'est écrit dans le code.
//
// Échange avec vibelearn par des fichiers du dossier .vibelearn/ui/ du projet :
// - state.json : écrit par vibelearn (tout ce qu'il faut afficher) ;
// - inbox/*.json : un fichier par message de l'apprenant, lu puis effacé par vibelearn ;
// - ack.json : la dernière version de state.json affichée ici ;
// - extension.log : les erreurs éventuelles de cette extension.
'use strict';

const vscode = require('vscode');
const fs = require('fs');
const path = require('path');

const VERSION = '__VERSION__';
// Texts shown when there is no session to take them from: French in a French editor, English otherwise.
const FRENCH = typeof vscode.env.language === 'string' && vscode.env.language.toLowerCase().startsWith('fr');
const say = (fr, en) => (FRENCH ? fr : en);
/** @type {Map<string, Session>} racine du projet -> session */
const sessions = new Map();
let statusBar;
let output;
/** Le dernier éditeur de code actif : le panneau prend le focus quand on y tape. */
let lastCodeEditor;
const deco = {};

function log(message) {
  const line = `[${new Date().toLocaleTimeString()}] ${message}`;
  if (output) output.appendLine(line);
  for (const s of sessions.values()) {
    try {
      fs.appendFileSync(path.join(s.uiDir, 'extension.log'), line + '\n');
    } catch {
      // dossier disparu : rien à faire
    }
  }
}

function samePath(a, b) {
  const norm = (p) => path.normalize(p).replace(/[\\/]+$/, '');
  return process.platform === 'win32' ? norm(a).toLowerCase() === norm(b).toLowerCase() : norm(a) === norm(b);
}

function inside(root, file) {
  const rel = path.relative(root, file);
  return !!rel && !rel.startsWith('..') && !path.isAbsolute(rel);
}

function sessionFor(uri) {
  for (const s of sessions.values()) if (s.live() && inside(s.root, uri.fsPath)) return s;
  return undefined;
}

/** La session à laquelle s'adressent les commandes : celle du fichier ouvert, sinon la seule active. */
function current() {
  const editor = lastCodeEditor || vscode.window.activeTextEditor;
  if (editor) {
    const s = sessionFor(editor.document.uri);
    if (s) return s;
  }
  for (const s of sessions.values()) if (s.live()) return s;
  return undefined;
}

/** La ligne (indice) qui contient `anchor`, la plus proche de `near`. */
function findNear(document, anchor, near) {
  const squash = (s) => s.trim().split(/\s+/).join(' ');
  const wanted = squash(anchor || '');
  const clamp = (n) => Math.max(0, Math.min(n, document.lineCount - 1));
  if (!wanted) return clamp(near);
  let best = -1;
  for (const exact of [true, false]) {
    for (let i = 0; i < document.lineCount; i++) {
      const line = squash(document.lineAt(i).text);
      if (exact ? line === wanted : line.includes(wanted)) {
        if (best < 0 || Math.abs(i - near) < Math.abs(best - near)) best = i;
      }
    }
    if (best >= 0) return best;
  }
  return clamp(near);
}

/** Texte brut d'une ligne de markdown, pour le bout de ligne. */
function plain(text, max) {
  const first = String(text || '').split('\n').find((l) => l.trim()) || '';
  const flat = first.replace(/\*\*|__|`/g, '').replace(/^\s*[-*]\s+/, '').trim();
  return flat.length > max ? flat.slice(0, max - 1) + '…' : flat;
}

// ---------------------------------------------------------------------------
// Repères dans l'éditeur
// ---------------------------------------------------------------------------

function svgIcon(body, color) {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="${color}" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
  return vscode.Uri.parse('data:image/svg+xml;base64,' + Buffer.from(svg).toString('base64'));
}

/** Marque de chaque ton d'annotation en bout de ligne (les tons viennent de `threads::TONES`). */
const TONE_MARKS = { ok: '✓ ', fix: '⚠ ', design: '⊞ ', info: '› ' };
const DESIGN_COLOR = '#D4537E';
/** Couleur des corrections faites par vibelearn (mode assistance). */
const ASSIST_COLOR = '#3AB7C8';
/** Couleur de la prochaine étape demandée par `//n …`. */
const NEXT_COLOR = '#2FA67A';
/** Flèche qui avance vers un repère : la prochaine étape. */
/** Couleur du pseudo-code demandé par `//// …`. */
const PSEUDO_COLOR = '#B07CE8';
const PSEUDO_ICON = '<path d="M5 4L2 8l3 4"/><path d="M11 4l3 4-3 4"/><path d="M7 8h.01M9 8h.01"/>';
/** Teinte du bloc de pseudo-code ; `color-mix` l'accorde au thème, clair ou sombre. */
const PSEUDO_TINT = '#A678F0';
/** Mots-clés mis à part en tête d'une ligne de pseudo-code formel (`////!`). */
const PSEUDO_KEYWORDS =
  /^(pour chaque|tant que|sinon si|for each|else if|fonction|function|retourner|retourne|renvoyer|renvoie|return|répéter|repeat|sinon|while|else|pour|for|si|if)(?=$|[\s:(])/i;

/**
 * Le pseudo-code du modèle, rangé pour l'affichage : le titre (`[where]`, quand
 * le code va ailleurs), ce qu'il faut créer (`[add]`, marqué `+`), puis les
 * lignes. En phrases (`////`) les étapes sont numérotées ; en formel (`////!`)
 * le mot-clé de tête est mis à part. Le retrait d'une ligne est son imbrication.
 */
function pseudoRows(text, formal) {
  const rows = [];
  let step = 0;
  let part = 'main';
  let last = '';
  for (const raw of String(text || '').split('\n')) {
    if (!raw.trim()) continue;
    const depth = Math.min(4, Math.floor(raw.match(/^[ \t]*/)[0].replace(/\t/g, '  ').length / 2));
    let body = raw.trim();
    const tag = body.match(/^\[([a-z]+)\]\s*/i);
    const kind = tag ? tag[1].toLowerCase() : '';
    if (tag) body = body.slice(tag[0].length);
    // Le modèle a numéroté lui-même : c'est l'éditeur qui numérote.
    body = body.replace(/^\d+[.)]\s+/, '');
    let row;
    if (kind === 'where' && !rows.length) row = { head: true, mark: '', kw: '', text: body, pad: 0, part: 'head' };
    else if (kind === 'add' || kind === 'field') row = { add: true, mark: '+', kw: '', text: body, pad: 0, part: 'add' };
    else {
      const kw = formal ? (body.match(PSEUDO_KEYWORDS) || [''])[0] : '';
      // Une fonction utilitaire et son corps forment une partie à eux seuls.
      if (!depth) part = /^(fonction|function)$/i.test(kw) ? 'fn' + rows.length : 'main';
      const rest = kw ? body.slice(kw.length).trimStart() : body;
      row = { mark: !formal && !depth ? String(++step) : '', kw, text: kw && rest ? ' ' + rest : rest, pad: depth * (formal ? 4 : 2), part };
    }
    // Un filet sépare les parties (sauf sous le titre, déjà sur son bandeau).
    row.sep = rows.length > 0 && row.part !== last && last !== 'head';
    last = row.part;
    rows.push(row);
  }
  return rows;
}

/**
 * Les deux boîtes de chaque ligne du bloc, larges ensemble du même nombre de
 * caractères : la marge (`+`, numéro, ou mot-clé avec son retrait), puis le texte.
 */
function pseudoCells(rows, formal) {
  const gutter = formal ? 3 : 4;
  const width = Math.min(100, Math.max(...rows.map((r) => gutter + r.pad + r.kw.length + r.text.length)) + 2);
  return rows.map((r) => {
    const first = r.kw
      ? { text: r.kw, width: gutter + r.pad + r.kw.length, left: gutter + r.pad, right: 0, accent: true, bold: true }
      : { text: r.mark || ' ', width: gutter, left: 0, right: 1, alignRight: true, accent: !r.add, add: r.add, bold: r.add };
    const second = { text: r.text || ' ', width: Math.max(1, width - first.width), left: r.kw ? 0 : r.pad, right: 2, bold: r.head, dim: r.dim };
    return [first, second];
  });
}

/**
 * Le CSS d'une boîte, glissé par `textDecoration` (VS Code le recopie tel quel) :
 * largeur fixe, fond commun, et le contour du bloc dessiné bord à bord.
 */
function pseudoCss(cell, o) {
  const tint = (pct) => `color-mix(in srgb, ${PSEUDO_TINT} ${pct}%, transparent)`;
  const shadows = [o.first ? `inset 1px 0 0 ${tint(38)}` : `inset -1px 0 0 ${tint(38)}`];
  if (o.top) shadows.push(`inset 0 1px 0 ${tint(38)}`);
  else if (o.sep) shadows.push(`inset 0 1px 0 ${tint(24)}`);
  if (o.bottom) shadows.push(`inset 0 -1px 0 ${tint(38)}`);
  const top = o.top ? 5 : 0;
  const bottom = o.bottom ? 5 : 0;
  const color = cell.add
    ? 'var(--vscode-gitDecoration-addedResourceForeground, #73c991)'
    : cell.accent
      ? `color-mix(in srgb, ${PSEUDO_TINT} 72%, var(--vscode-editor-foreground))`
      : cell.dim
        ? 'var(--vscode-descriptionForeground)'
        : 'var(--vscode-editor-foreground)';
  return (
    [
      'none',
      'display: inline-block',
      'box-sizing: border-box',
      'vertical-align: top',
      'overflow: hidden',
      'white-space: pre',
      'font-style: normal',
      `width: ${cell.width}ch`,
      `padding: 0 ${cell.right}ch 0 ${cell.left}ch`,
      `margin: 0 0 0 ${o.margin || 0}ch`,
      `text-align: ${cell.alignRight ? 'right' : 'left'}`,
      `font-weight: ${cell.bold ? 600 : 400}`,
      `color: ${color}`,
      `background: color-mix(in srgb, var(--vscode-editor-background), ${PSEUDO_TINT} ${o.head ? 18 : 9}%)`,
      `box-shadow: ${shadows.join(', ')}`,
      `border-radius: ${o.first ? `${top}px 0 0 ${bottom}px` : `0 ${top}px ${bottom}px 0`}`,
    ].join('; ') + ';'
  );
}

const NEXT_ICON = '<path d="M2.5 8h9"/><path d="M8.5 4.5L12 8l-3.5 3.5"/><path d="M14 3v10"/>';
const PENCIL = '<path d="M3 13l.8-3.2L11 2.6l2.4 2.4-7.2 7.2z"/>';
/** Blocs avec un emplacement libre : on ajoute une pièce sans toucher aux autres. */
const DESIGN_ICON =
  '<rect x="2" y="2" width="5" height="5" rx=".5"/><rect x="9" y="2" width="5" height="5" rx=".5"/><rect x="2" y="9" width="5" height="5" rx=".5"/><path d="M11.5 9.5v4M9.5 11.5h4"/>';

function createDecorations() {
  const after = (color) => ({
    margin: '0 0 0 2.5em',
    color: color.startsWith('#') ? color : new vscode.ThemeColor(color),
    fontStyle: 'italic',
  });
  deco.target = vscode.window.createTextEditorDecorationType({
    isWholeLine: true,
    backgroundColor: 'rgba(55, 138, 221, 0.14)',
    overviewRulerColor: 'rgba(55, 138, 221, 0.9)',
    overviewRulerLane: vscode.OverviewRulerLane.Full,
    gutterIconPath: svgIcon('<path d="M3 13l.8-3.2L11 2.6l2.4 2.4-7.2 7.2z"/>', '#378ADD'),
    gutterIconSize: '75%',
    after: after('textLink.foreground'),
  });
  deco.ok = vscode.window.createTextEditorDecorationType({
    gutterIconPath: svgIcon('<path d="M3 8.5l3 3 7-7"/>', '#3FA15B'),
    gutterIconSize: '75%',
    after: after('testing.iconPassed'),
  });
  deco.fix = vscode.window.createTextEditorDecorationType({
    isWholeLine: true,
    backgroundColor: 'rgba(239, 159, 39, 0.12)',
    overviewRulerColor: 'rgba(239, 159, 39, 0.9)',
    overviewRulerLane: vscode.OverviewRulerLane.Right,
    gutterIconPath: svgIcon('<path d="M8 2l6.5 11.5h-13z"/><path d="M8 6.5v3"/><path d="M8 11.8v.2"/>', '#E0A100'),
    gutterIconSize: '75%',
    after: after('editorWarning.foreground'),
  });
  deco.design = vscode.window.createTextEditorDecorationType({
    gutterIconPath: svgIcon(DESIGN_ICON, DESIGN_COLOR),
    gutterIconSize: '75%',
    after: after(DESIGN_COLOR),
  });
  deco.info = vscode.window.createTextEditorDecorationType({
    gutterIconPath: svgIcon('<circle cx="8" cy="8" r="2.5"/>', '#7F77DD'),
    gutterIconSize: '75%',
    after: after('editorInfo.foreground'),
  });
  deco.next = vscode.window.createTextEditorDecorationType({
    isWholeLine: true,
    backgroundColor: 'rgba(47, 166, 122, 0.10)',
    overviewRulerColor: 'rgba(47, 166, 122, 0.9)',
    overviewRulerLane: vscode.OverviewRulerLane.Right,
    gutterIconPath: svgIcon(NEXT_ICON, NEXT_COLOR),
    gutterIconSize: '75%',
    after: { margin: '0 0 0 2.5em', color: NEXT_COLOR, fontStyle: 'normal' },
  });
  // `//// …` : le pseudo-code, une ligne de l'éditeur par ligne, en colonne à droite.
  deco.pseudoHead = vscode.window.createTextEditorDecorationType({
    gutterIconPath: svgIcon(PSEUDO_ICON, PSEUDO_COLOR),
    gutterIconSize: '75%',
    overviewRulerColor: 'rgba(176, 124, 232, 0.9)',
    overviewRulerLane: vscode.OverviewRulerLane.Right,
  });
  // Le bloc de pseudo-code : chaque ligne porte ses deux boîtes (voir `pseudoColumn`).
  deco.pseudo = vscode.window.createTextEditorDecorationType({});
  deco.edit = vscode.window.createTextEditorDecorationType({
    isWholeLine: true,
    backgroundColor: 'rgba(58, 183, 200, 0.10)',
    overviewRulerColor: 'rgba(58, 183, 200, 0.9)',
    overviewRulerLane: vscode.OverviewRulerLane.Right,
    gutterIconPath: svgIcon(PENCIL, ASSIST_COLOR),
    gutterIconSize: '75%',
    after: after(ASSIST_COLOR),
  });
}

/** `text` (lignes séparées par \n) avec les fins de ligne du document. */
function withEol(document, text) {
  return String(text).split('\n').join(document.eol === vscode.EndOfLine.CRLF ? '\r\n' : '\n');
}

/** Le passage `passage` qui commence le plus près de la ligne `near` (à moins de `slack` lignes), en un seul exemplaire sur sa ligne ; null sinon. */
function findPassage(document, passage, near, slack) {
  const needle = withEol(document, passage);
  if (!needle) return null;
  const text = document.getText();
  const hits = [];
  for (let i = text.indexOf(needle); i >= 0; i = text.indexOf(needle, i + 1)) hits.push({ at: i, line: document.positionAt(i).line });
  const close = hits.filter((h) => Math.abs(h.line - near) <= slack).sort((a, b) => Math.abs(a.line - near) - Math.abs(b.line - near));
  if (!close.length || close.filter((h) => h.line === close[0].line).length > 1) return null;
  return new vscode.Range(document.positionAt(close[0].at), document.positionAt(close[0].at + needle.length));
}

/** Un passage sur une ligne, pour le bout de ligne et le panneau. */
function oneLine(text) {
  return String(text).split('\n').map((l) => l.trim()).filter(Boolean).join(' ⏎ ');
}

/** La ligne dont le texte est exactement `anchor` (aux espaces près), la plus proche de `near` ; -1 si elle n'existe plus. */
function findExact(document, anchor, near) {
  const squash = (x) => x.trim().split(/\s+/).join(' ');
  const wanted = squash(anchor || '');
  if (!wanted) return Math.max(0, Math.min(near, document.lineCount - 1));
  let best = -1;
  for (let i = 0; i < document.lineCount; i++) {
    if (squash(document.lineAt(i).text) === wanted && (best < 0 || Math.abs(i - near) < Math.abs(best - near))) best = i;
  }
  return best;
}

/** Le survol d'un passage corrigé : pourquoi, avant/après, et le lien pour annuler. */
function editHover(s, e, doc, r, strings) {
  let diff;
  if (r.isSingleLine && !e.old.includes('\n')) {
    const line = doc.lineAt(r.start.line).text;
    const before = line.slice(0, r.start.character) + e.old + line.slice(r.end.character);
    diff = `- ${before.trim()}\n+ ${line.trim()}`;
  } else {
    diff = e.old.split('\n').map((l) => `- ${l}`).concat(e.new.split('\n').map((l) => `+ ${l}`)).join('\n');
  }
  const args = encodeURIComponent(JSON.stringify([s.root, e.id]));
  const md = new vscode.MarkdownString();
  md.isTrusted = { enabledCommands: ['vibelearn.undoEdit'] };
  md.appendMarkdown(`**${strings.edit_hover || 'vibelearn changed this line'}**\n\n`);
  md.appendText(e.reason);
  md.appendCodeblock(diff, 'diff');
  md.appendMarkdown(`[${strings.undo_hover || 'Undo this change'}](command:vibelearn.undoEdit?${args}) (${strings.undo_also || 'Ctrl+Z works too'})`);
  return md;
}

/** Les modes sans étape, où vibelearn regarde chaque sauvegarde. */
function watchMode(st) {
  return st.mode === 'monitor' || st.mode === 'assist';
}

/** Le survol d'une remarque ; en surveillance : « Comment ? » et « Apprends-moi » (hors leçon), et « Oublie ». */
function remarkHover(s, a, strings) {
  const st = s.state;
  // Le titre en gras, puis l'explication.
  const [head, ...rest] = String(a.text || '').split(/\n\s*\n/);
  const text = `**${head.trim()}**` + (rest.length ? '\n\n' + rest.join('\n\n') : '');
  if (!watchMode(st)) return new vscode.MarkdownString(text);
  const args = encodeURIComponent(JSON.stringify([s.root, a.id]));
  const link = (label, command) => `[${label}](command:${command}?${args})`;
  const links = st.step ? [] : [link(strings.how || 'How?', 'vibelearn.how'), link(strings.teach || 'Teach me', 'vibelearn.teach')];
  links.push(link(strings.forget || 'Forget', 'vibelearn.forget'));
  const md = new vscode.MarkdownString(text);
  md.isTrusted = { enabledCommands: ['vibelearn.how', 'vibelearn.teach', 'vibelearn.forget'] };
  md.appendMarkdown('\n\n---\n\n' + links.join('  \n'));
  return md;
}

/** Le survol d'une prochaine étape : la fonctionnalité, l'étape en entier, et « Efface ». */
function nextHover(s, n, strings) {
  const args = encodeURIComponent(JSON.stringify([s.root, n.id]));
  const md = new vscode.MarkdownString();
  md.isTrusted = { enabledCommands: ['vibelearn.forget'] };
  md.appendMarkdown(`**${strings.next_hover || 'Next step'}**${n.feature ? strings.colon || ': ' : ''}`);
  if (n.feature) md.appendText(n.feature);
  md.appendMarkdown('\n\n' + String(n.text || ''));
  md.appendMarkdown(`\n\n---\n\n[${strings.next_clear || 'Clear'}](command:vibelearn.forget?${args})`);
  return md;
}

/** Le survol d'un pseudo-code : la demande, le pseudo-code entier, et « Efface ». */
function pseudoHover(s, p, strings) {
  const args = encodeURIComponent(JSON.stringify([s.root, p.id]));
  const formal = p.style === 'formal';
  const md = new vscode.MarkdownString();
  md.isTrusted = { enabledCommands: ['vibelearn.forget'] };
  md.appendMarkdown(`**${strings.pseudo_hover || 'Pseudo-code'}**${p.request ? strings.colon || ': ' : ''}`);
  if (p.request) md.appendText(p.request);
  const lines = pseudoRows(p.text, formal).map((r) => {
    if (r.head) return r.text;
    if (r.add) return '+ ' + r.text;
    if (formal) return ' '.repeat(r.pad) + r.kw + r.text;
    return (r.mark ? r.mark + '. ' : '   ' + ' '.repeat(r.pad)) + r.text;
  });
  md.appendCodeblock(lines.join('\n'), 'text');
  md.appendMarkdown(`[${strings.next_clear || 'Clear'}](command:vibelearn.forget?${args})`);
  return md;
}

/** Largeur affichée d'une ligne (tabulations comprises). */
function displayWidth(text, tabSize) {
  let w = 0;
  for (const c of text) w = c === '\t' ? w + tabSize - (w % tabSize) : w + 1;
  return w;
}

/**
 * Le pseudo-code `p` en bloc, à partir de la ligne `start` : une ligne du bloc
 * au bout de chaque ligne de l'éditeur, toutes à la même colonne et de la même
 * largeur. Près de la fin du fichier, le bloc remonte pour tenir en entier ; ce
 * qui ne tient toujours pas est résumé (le tout au survol).
 */
function pseudoColumn(editor, start, p, hover) {
  const doc = editor.document;
  const tabSize = Number(editor.options.tabSize) || 4;
  const formal = p.style === 'formal';
  let rows = pseudoRows(p.text, formal);
  if (!rows.length) return [];
  start = Math.max(0, Math.min(start, doc.lineCount - rows.length));
  const room = doc.lineCount - start;
  if (rows.length > room) {
    const more = rows.length - room + 1;
    rows = rows.slice(0, Math.max(0, room - 1)).concat([{ mark: '', kw: '', text: `… (+${more})`, pad: 0, dim: true }]);
  }
  const cells = pseudoCells(rows, formal);
  const widths = rows.map((_, i) => displayWidth(doc.lineAt(start + i).text, tabSize));
  const column = Math.max(...widths) + 4;
  return rows.map((r, i) => {
    const end = doc.lineAt(start + i).range.end;
    const o = { head: r.head, sep: r.sep, top: i === 0, bottom: i === rows.length - 1 };
    const [first, second] = cells[i];
    return {
      range: new vscode.Range(end, end),
      hoverMessage: hover,
      renderOptions: {
        before: { contentText: first.text, textDecoration: pseudoCss(first, { ...o, first: true, margin: column - widths[i] }) },
        after: { contentText: second.text, textDecoration: pseudoCss(second, { ...o, first: false }) },
      },
    };
  });
}

function applyDecorations(editor) {
  const sets = { target: [], edit: [], next: [], pseudo: [], pseudoHead: [] };
  for (const tone of Object.keys(TONE_MARKS)) sets[tone] = [];
  const s = sessionFor(editor.document.uri);
  if (s && s.state) {
    const st = s.state;
    const doc = editor.document;
    const file = doc.uri.fsPath;
    const range = (n) => doc.lineAt(Math.max(0, Math.min(n, doc.lineCount - 1))).range;
    const strings = st.strings || {};
    if (st.step && st.step.file && samePath(st.step.file, file)) {
      const line = findNear(doc, st.step.anchor, st.step.line);
      const label = '✎ ' + (st.step.here || strings.write_here || 'write here');
      sets.target.push({
        range: range(line),
        hoverMessage: new vscode.MarkdownString(`**${st.step.title}**\n\n${st.step.goal}`),
        renderOptions: { after: { contentText: plain(label, 80) } },
      });
    }
    for (const a of st.annotations || []) {
      if (!samePath(a.file, file)) continue;
      // La ligne a changé : l'annotation parlait d'un code qui n'existe plus.
      const line = findExact(doc, a.anchor, a.line);
      if (line < 0) continue;
      const tone = a.tone in TONE_MARKS ? a.tone : 'info';
      const mark = TONE_MARKS[tone];
      sets[tone].push({
        range: range(line),
        hoverMessage: remarkHover(s, a, strings),
        renderOptions: { after: { contentText: mark + plain(a.text, 90) } },
      });
    }
    // `//n …` : la prochaine étape, au bout de la ligne où elle a été demandée.
    for (const n of st.next_steps || []) {
      if (!samePath(n.file, file)) continue;
      const line = findNear(doc, n.anchor, n.line);
      const first = String(n.text || '').split('\n').map((l) => l.trim()).find(Boolean) || '';
      sets.next.push({
        range: range(line),
        hoverMessage: nextHover(s, n, strings),
        renderOptions: { after: { contentText: '→ ' + plain(first, 110) } },
      });
    }
    // `//// …` : le pseudo-code à côté de la demande, qui reste dans le code.
    for (const p of st.pseudo || []) {
      if (!samePath(p.file, file)) continue;
      // La demande a été modifiée ou retirée : le pseudo-code ne lui correspond plus.
      const line = findExact(doc, p.anchor, p.line);
      if (line < 0) continue;
      const hover = pseudoHover(s, p, strings);
      sets.pseudoHead.push({ range: range(line), hoverMessage: hover });
      // À partir de la ligne sous la demande (sur elle si elle termine le fichier).
      const below = line + 1 < doc.lineCount ? line + 1 : line;
      sets.pseudo.push(...pseudoColumn(editor, below, p, hover));
    }
    // Mode assistance : les lignes que vibelearn a corrigées, tant qu'elles portent la correction.
    for (const e of st.edits || []) {
      if (e.status !== 'applied' || !samePath(e.file, file)) continue;
      const r = findPassage(doc, e.new, e.line, 40);
      if (!r) continue;
      const text = `✎ vibelearn : ${oneLine(e.old)} → ${oneLine(e.new)}, ${e.reason}`;
      sets.edit.push({
        range: new vscode.Range(r.start.line, 0, r.end.line, doc.lineAt(r.end.line).text.length),
        hoverMessage: editHover(s, e, doc, r, strings),
        renderOptions: { after: { contentText: plain(text, 90) } },
      });
    }
  }
  for (const key of Object.keys(sets)) editor.setDecorations(deco[key], sets[key]);
}

function applyAll() {
  for (const editor of vscode.window.visibleTextEditors) applyDecorations(editor);
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

class Session {
  constructor(statePath) {
    this.statePath = statePath;
    this.uiDir = path.dirname(statePath);
    this.root = path.dirname(path.dirname(this.uiDir));
    this.inbox = path.join(this.uiDir, 'inbox');
    this.mtime = 0;
    this.state = null;
    this.panel = null;
    /** l'apprenant a fermé le panneau : on ne le rouvre que pour une nouvelle étape ou une question */
    this.panelClosed = false;
    this.lastPrompt = null;
    this.lastStep = null;
    this.revealSeq = 0;
    this.warnedStale = false;
    this.warnedError = false;
    /** corrections déjà prises en charge (mode assistance) */
    this.handled = new Set();
    this.undoing = new Set();
  }

  live() {
    return !!this.state && this.state.status !== 'ended';
  }

  poll() {
    let stat;
    try {
      stat = fs.statSync(this.statePath);
    } catch {
      return;
    }
    if (stat.mtimeMs !== this.mtime) {
      this.mtime = stat.mtimeMs;
      this.load();
    }
  }

  load() {
    let state;
    try {
      state = JSON.parse(fs.readFileSync(this.statePath, 'utf8'));
    } catch {
      return; // en cours d'écriture : on relira au prochain passage
    }
    if (this.state && state.version === this.state.version) return;
    this.state = state;
    // vibelearn a installé une autre version de cette extension : VS Code ne la charge qu'au rechargement.
    if (state.extension && state.extension !== VERSION && !this.warnedStale) {
      this.warnedStale = true;
      log(`version ${VERSION} chargée, vibelearn attend ${state.extension}`);
      offerReload(say('vibelearn a été mis à jour : recharge la fenêtre pour afficher la nouvelle version.', 'vibelearn was updated: reload the window to show the new version.'));
    }
    let error = null;
    try {
      this.render();
    } catch (e) {
      error = String(e && e.message ? e.message : e);
      log(`affichage : ${e && e.stack ? e.stack : e}`);
      if (!this.warnedError) {
        this.warnedError = true;
        offerReload(say(`vibelearn n'arrive pas à s'afficher (${error}). Recharger la fenêtre peut suffire.`, `vibelearn cannot display itself (${error}). Reloading the window may be enough.`));
      }
    }
    try {
      fs.writeFileSync(path.join(this.uiDir, 'ack.json'), JSON.stringify({ version: state.version, extension: VERSION, error }));
    } catch (e) {
      log(`ack : ${e}`);
    }
  }

  send(message) {
    try {
      fs.mkdirSync(this.inbox, { recursive: true });
      const name = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const tmp = path.join(this.inbox, `${name}.tmp`);
      fs.writeFileSync(tmp, JSON.stringify(message));
      fs.renameSync(tmp, path.join(this.inbox, `${name}.json`));
    } catch (e) {
      log(`envoi : ${e}`);
      vscode.window.showErrorMessage(`vibelearn: ${e}`);
    }
  }

  render() {
    const st = this.state;
    const live = this.live();
    vscode.commands.executeCommand('setContext', 'vibelearn.active', !!current());
    // Le panneau s'ouvre au début, et revient pour une nouvelle étape ou une question.
    const prompt = st.prompt ? st.prompt.id : null;
    const step = st.step ? st.step.title : null;
    const comeBack = (prompt && prompt !== this.lastPrompt) || (step && step !== this.lastStep);
    this.lastPrompt = prompt;
    this.lastStep = step;
    if (live && (!this.panelClosed || comeBack)) this.showPanel(false);
    this.post();
    applyAll();
    updateStatus();
    if (live) this.reveal();
    if (live) this.applyEdits();
  }

  /** Mode assistance : applique dans l'éditeur les corrections que vibelearn vient de décider. */
  applyEdits() {
    for (const e of this.state.edits || []) {
      if (e.status !== 'pending' || this.handled.has(e.id)) continue;
      this.handled.add(e.id);
      this.applyEdit(e).catch((err) => {
        log(`correction ${e.id} : ${err && err.stack ? err.stack : err}`);
        this.send({ type: 'edit_result', id: e.id, ok: false, error: String(err) });
      });
    }
  }

  async applyEdit(e) {
    const fail = (error) => this.send({ type: 'edit_result', id: e.id, ok: false, error });
    const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(e.file));
    // Le développeur est en train d'écrire dans ce fichier : on n'y touche pas.
    if (doc.isDirty) return fail('the file has unsaved changes in the editor: the developer is writing in it');
    const r = findPassage(doc, e.old, e.line, 3);
    if (!r) return fail(`\`${oneLine(e.old)}\` no longer starts at line ${e.line + 1}`);
    const edit = new vscode.WorkspaceEdit();
    edit.replace(doc.uri, r, withEol(doc, e.new));
    if (!(await vscode.workspace.applyEdit(edit))) return fail('the editor refused the edit');
    await doc.save();
    log(`corrigé : ${path.basename(e.file)}:${r.start.line + 1} ${oneLine(e.old)} → ${oneLine(e.new)}`);
    this.send({ type: 'edit_result', id: e.id, ok: true, line: r.start.line });
  }

  /** Annule une correction (bouton du panneau ou lien du survol). */
  async undoEdit(id) {
    const e = ((this.state && this.state.edits) || []).find((x) => x.id === id && x.status === 'applied');
    if (!e || this.undoing.has(id)) return;
    this.undoing.add(id);
    try {
      const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(e.file));
      const r = findPassage(doc, e.new, e.line, 40);
      if (!r) {
        const strings = this.state.strings || {};
        vscode.window.showInformationMessage(`vibelearn${strings.colon || ': '}${strings.undo_gone || 'this line has changed since: nothing to undo.'}`);
        return;
      }
      const wasDirty = doc.isDirty;
      const edit = new vscode.WorkspaceEdit();
      edit.replace(doc.uri, r, withEol(doc, e.old));
      if (!(await vscode.workspace.applyEdit(edit))) return;
      // Un fichier en cours d'écriture garde l'annulation pour sa prochaine sauvegarde.
      if (!wasDirty) await doc.save();
      this.send({ type: 'edit_undone', id, saved: !wasDirty });
    } finally {
      this.undoing.delete(id);
    }
  }

  showPanel(focus) {
    if (this.panel) {
      if (focus) this.panel.reveal(this.panel.viewColumn, false);
      return;
    }
    this.panelClosed = false;
    const besideCode = vscode.window.activeTextEditor ? vscode.ViewColumn.Beside : vscode.ViewColumn.Two;
    this.panel = vscode.window.createWebviewPanel(
      'vibelearn',
      'vibelearn',
      { viewColumn: besideCode, preserveFocus: !focus },
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [] },
    );
    this.panel.webview.html = panelHtml();
    this.panel.onDidDispose(() => {
      this.panel = null;
      this.panelClosed = true;
    });
    this.panel.webview.onDidReceiveMessage((m) => {
      try {
        this.fromPanel(m);
      } catch (e) {
        log(`panneau : ${e && e.stack ? e.stack : e}`);
      }
    });
  }

  /** La ligne où est le curseur, pour « Pose ta question ». */
  context() {
    const e = lastCodeEditor;
    if (!e || !inside(this.root, e.document.uri.fsPath)) return null;
    return { file: path.basename(e.document.uri.fsPath), line: e.selection.active.line + 1 };
  }

  post() {
    if (this.panel && this.state) this.panel.webview.postMessage({ type: 'state', state: this.state, context: this.context() });
  }

  fromPanel(m) {
    switch (m.type) {
      case 'ready':
        this.post();
        break;
      case 'answer':
        this.send(Object.assign({ type: 'answer', prompt: m.prompt }, m.answer));
        break;
      case 'command':
        this.send({ type: 'command', name: m.name });
        break;
      case 'ask': {
        const message = { type: 'ask', text: m.text };
        const e = lastCodeEditor;
        if (e && inside(this.root, e.document.uri.fsPath)) {
          message.file = e.document.uri.fsPath;
          message.line = e.selection.active.line;
          const selected = e.document.getText(e.selection);
          if (selected.trim()) message.selection = selected.slice(0, 1500);
        }
        this.send(message);
        break;
      }
      case 'goto':
        this.goto(m.file, m.line, m.anchor);
        break;
      case 'undo':
        this.undoEdit(m.id).catch((e) => log(`annulation : ${e}`));
        break;
      case 'teach':
      case 'forget':
        this.send({ type: m.type, id: m.id });
        break;
      case 'where':
        if (this.state.step && this.state.step.file) this.goto(this.state.step.file, this.state.step.line, this.state.step.anchor);
        break;
      default:
        log(`message inconnu du panneau : ${m.type}`);
    }
  }

  /** Ouvre le fichier dans la colonne du code (pas dans celle du panneau) et va à la ligne. */
  async goto(file, line, anchor) {
    try {
      const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
      const column = lastCodeEditor && lastCodeEditor.viewColumn ? lastCodeEditor.viewColumn : vscode.ViewColumn.One;
      const editor = await vscode.window.showTextDocument(doc, { viewColumn: column, preview: false });
      const l = findNear(doc, anchor, line);
      const pos = new vscode.Position(l, doc.lineAt(l).firstNonWhitespaceCharacterIndex);
      editor.selection = new vscode.Selection(pos, pos);
      editor.revealRange(new vscode.Range(pos, pos), vscode.TextEditorRevealType.InCenterIfOutsideViewport);
    } catch (e) {
      log(`ouverture de ${file} : ${e}`);
    }
  }

  reveal() {
    const r = this.state.reveal;
    if (!r || r.seq <= this.revealSeq) return;
    this.revealSeq = r.seq;
    const anchor = this.state.step && this.state.step.file && samePath(this.state.step.file, r.file) ? this.state.step.anchor : null;
    this.goto(r.file, r.line, anchor);
  }
}

function offerReload(message) {
  vscode.window.showWarningMessage(message, say('Recharger la fenêtre', 'Reload window')).then((choice) => {
    if (choice) vscode.commands.executeCommand('workbench.action.reloadWindow');
  });
}

function updateStatus() {
  const s = current();
  if (!s || !s.state || !s.live()) {
    statusBar.hide();
    return;
  }
  const st = s.state;
  const busy = st.status === 'busy' || st.status === 'starting';
  const strings = st.strings || {};
  const colon = strings.colon || ': ';
  statusBar.text = st.prompt
    ? `$(bell) vibelearn${colon}${strings.waiting || 'waiting for your answer'}`
    : `${busy ? '$(sync~spin)' : '$(mortar-board)'} vibelearn${colon}${st.status_text || ''}`;
  statusBar.tooltip = st.topic;
  statusBar.command = 'vibelearn.show';
  statusBar.show();
}

function attach(statePath) {
  const root = path.dirname(path.dirname(path.dirname(statePath)));
  let s = sessions.get(root.toLowerCase());
  if (!s) {
    s = new Session(statePath);
    sessions.set(root.toLowerCase(), s);
    log(`session trouvée : ${root}`);
  }
  s.poll();
}

function discover() {
  for (const folder of vscode.workspace.workspaceFolders || []) {
    const statePath = path.join(folder.uri.fsPath, '.vibelearn', 'ui', 'state.json');
    if (fs.existsSync(statePath)) attach(statePath);
  }
}

function activate(context) {
  output = vscode.window.createOutputChannel('vibelearn');
  statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  createDecorations();
  context.subscriptions.push(output, statusBar, ...Object.values(deco));
  if (vscode.window.activeTextEditor) lastCodeEditor = vscode.window.activeTextEditor;

  let retrack = null;
  let contextTimer = null;
  const sendContext = () => {
    clearTimeout(contextTimer);
    contextTimer = setTimeout(() => {
      for (const s of sessions.values()) {
        if (s.panel) s.panel.webview.postMessage({ type: 'context', context: s.context() });
      }
    }, 150);
  };
  const withSession = (fn) => () => {
    const s = current();
    if (s) fn(s);
    else vscode.window.showInformationMessage(say('vibelearn : aucune session en cours dans ce dossier.', 'vibelearn: no session running in this folder.'));
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('vibelearn.show', withSession((s) => s.showPanel(true))),
    vscode.commands.registerCommand('vibelearn.ask', withSession((s) => {
      s.showPanel(true);
      if (s.panel) s.panel.webview.postMessage({ type: 'focusAsk', context: s.context() });
    })),
    vscode.commands.registerCommand('vibelearn.hint', withSession((s) => s.send({ type: 'command', name: 'hint' }))),
    vscode.commands.registerCommand('vibelearn.check', withSession((s) => s.send({ type: 'command', name: 'check' }))),
    vscode.commands.registerCommand('vibelearn.how', (root, id) => {
      const s = root ? sessions.get(String(root).toLowerCase()) : current();
      if (s) s.send({ type: 'how', id });
    }),
    vscode.commands.registerCommand('vibelearn.teach', (root, id) => {
      const s = root ? sessions.get(String(root).toLowerCase()) : current();
      if (s) s.send({ type: 'teach', id });
    }),
    vscode.commands.registerCommand('vibelearn.forget', (root, id) => {
      const s = root ? sessions.get(String(root).toLowerCase()) : current();
      if (s) s.send({ type: 'forget', id });
    }),
    vscode.commands.registerCommand('vibelearn.undoEdit', (root, id) => {
      const s = root ? sessions.get(String(root).toLowerCase()) : current();
      if (s) s.undoEdit(id).catch((e) => log(`annulation : ${e}`));
    }),
    vscode.window.onDidChangeActiveTextEditor((e) => {
      if (e && e.document.uri.scheme === 'file') lastCodeEditor = e;
      sendContext();
      updateStatus();
    }),
    vscode.window.onDidChangeTextEditorSelection((e) => {
      if (e.textEditor.document.uri.scheme === 'file') lastCodeEditor = e.textEditor;
      sendContext();
    }),
    vscode.window.onDidChangeVisibleTextEditors(applyAll),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (!sessionFor(e.document.uri)) return;
      clearTimeout(retrack);
      retrack = setTimeout(applyAll, 120);
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(discover),
  );

  const watcher = vscode.workspace.createFileSystemWatcher('**/.vibelearn/ui/state.json');
  watcher.onDidCreate((uri) => attach(uri.fsPath));
  watcher.onDidChange((uri) => attach(uri.fsPath));
  context.subscriptions.push(watcher);
  discover();
  // Le relevé régulier rattrape ce que la surveillance de fichiers manquerait.
  const timer = setInterval(() => {
    discover();
    for (const s of sessions.values()) s.poll();
  }, 400);
  context.subscriptions.push({ dispose: () => clearInterval(timer) });
  log(`vibelearn ${VERSION} prêt`);
}

function deactivate() {
  for (const s of sessions.values()) if (s.panel) s.panel.dispose();
}

// ---------------------------------------------------------------------------
// Le panneau (page web dans VS Code)
// ---------------------------------------------------------------------------

function panelHtml() {
  const nonce = Math.random().toString(36).slice(2) + Math.random().toString(36).slice(2);
  return `<!DOCTYPE html>
<html lang="fr">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<style>${PANEL_CSS}</style>
</head>
<body>
<header class="bar" id="bar"></header>
<main class="body" id="main"></main>
<footer class="composer">
  <div class="help" id="help"></div>
  <form class="field" id="askform">
    <textarea id="ask" rows="1"></textarea>
    <div class="field-foot">
      <span class="ctx" id="ctx" hidden></span>
      <span class="keys" id="keys"></span>
      <button class="send" id="send" type="submit"></button>
    </div>
  </form>
</footer>
<script nonce="${nonce}">const KIT = (${webKit.toString()})();
(${panelMain.toString()})();</script>
</body>
</html>`;
}

// Les couleurs et les éléments de base des pages web.
const THEME_CSS = `
:root { color-scheme: light dark; }
* { box-sizing: border-box; }
html, body { height: 100%; }
body { margin: 0; padding: 0; font-family: var(--vscode-font-family); font-size: var(--vscode-font-size); line-height: 1.5; color: var(--vscode-foreground);
  --hair: color-mix(in srgb, var(--vscode-foreground) 13%, transparent); --edge: color-mix(in srgb, var(--vscode-foreground) 24%, transparent); --fill: color-mix(in srgb, var(--vscode-foreground) 8%, transparent);
  --desc: var(--vscode-descriptionForeground); --link: var(--vscode-textLink-foreground);
  --ok: var(--vscode-testing-iconPassed, #3FA15B); --fix: var(--vscode-editorWarning-foreground, #E0A100); --info: var(--vscode-editorInfo-foreground, #7F77DD); --design: #E06C98; --assist: #3AB7C8; }
body.vscode-light { --design: #B0336A; --assist: #0F7482; }
::selection { background: color-mix(in srgb, var(--vscode-focusBorder) 45%, transparent); }
:focus-visible { outline: 1px solid var(--vscode-focusBorder); outline-offset: 1px; }
button { font: inherit; color: inherit; }
button:disabled { opacity: .5; cursor: default; }
:where(p) { margin: 0; }
ul { margin: 0; padding-left: 18px; }
li + li { margin-top: 3px; }
code { font-family: var(--vscode-editor-font-family); font-size: .95em; background: var(--vscode-textCodeBlock-background); padding: 0 4px; border-radius: 3px; }
pre { margin: 0; padding: 8px 10px; border-radius: 6px; overflow-x: auto; background: var(--vscode-textCodeBlock-background); }
pre code { background: none; padding: 0; }
.md > * + * { margin-top: 8px; }
.i { flex: none; }
.tone { display: flex; margin-top: 3px; }
.tone.ok { color: var(--ok); } .tone.fix { color: var(--fix); } .tone.design { color: var(--design); } .tone.info { color: var(--info); } .tone.assist { color: var(--assist); }
.state { flex: none; width: 8px; height: 8px; border-radius: 50%; background: var(--ok); }
.state.wait { background: var(--link); }
.state.off { background: var(--edge); }
.state.busy, .spin { flex: none; width: 9px; height: 9px; border-radius: 50%; background: none; border: 1.5px solid var(--link); border-right-color: transparent; animation: spin .8s linear infinite; }
.spin { display: inline-block; border-color: currentColor; border-right-color: transparent; }
@keyframes spin { to { transform: rotate(360deg); } }
.link { border: 0; padding: 0; background: none; cursor: pointer; color: var(--link); }
.link:hover:enabled { text-decoration: underline; }
.loc { border: 0; padding: 0; background: none; cursor: pointer; font-variant-numeric: tabular-nums; }
.loc:hover { color: var(--vscode-foreground); text-decoration: underline; }
.icon-btn { flex: none; display: grid; place-items: center; width: 26px; height: 26px; border: 0; border-radius: 4px; background: none; cursor: pointer; transition: background-color .15s; }
.icon-btn:hover, .icon-btn[aria-expanded="true"] { background: var(--fill); }
.icon-btn[aria-pressed="true"] { background: var(--fill); color: var(--link); }
@media (prefers-reduced-motion: reduce) { * { animation: none !important; transition: none !important; } }
`;

// Trois zones fixes : l'état en haut, ce qui se lit au milieu (seul à défiler), le champ en bas.
const PANEL_CSS = THEME_CSS + `
body { display: grid; grid-template-rows: auto minmax(0, 1fr) auto; }

.bar { position: relative; z-index: 1; display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 12px; border-bottom: 1px solid var(--hair); }
.bar-text { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.bar-text strong { font-weight: 600; }
.bar-text span { color: var(--desc); margin-left: 8px; }
.btn { border: 0; border-radius: 4px; padding: 3px 10px; cursor: pointer; white-space: nowrap; background: var(--vscode-button-secondaryBackground); color: var(--vscode-button-secondaryForeground); transition: background-color .15s; }
.btn:hover:enabled { background: var(--vscode-button-secondaryHoverBackground); }
.btn.primary { background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
.btn.primary:hover:enabled { background: var(--vscode-button-hoverBackground); }
.menu { position: absolute; top: calc(100% - 2px); right: 8px; min-width: 220px; padding: 4px; border: 1px solid var(--vscode-menu-border, var(--edge)); border-radius: 6px; background: var(--vscode-menu-background, var(--vscode-editor-background)); box-shadow: 0 2px 8px rgba(0, 0, 0, .36); }
.menu-item { display: block; width: 100%; text-align: left; border: 0; border-radius: 4px; padding: 4px 10px; background: none; cursor: pointer; }
.menu-item:hover:enabled, .menu-item:focus-visible { background: var(--vscode-list-hoverBackground); }
.menu hr { border: 0; border-top: 1px solid var(--hair); margin: 4px 0; }
.menu-note { padding: 4px 10px 2px; font-size: .92em; color: var(--desc); }
.steps { position: absolute; left: 0; right: 0; bottom: -1px; display: flex; gap: 2px; height: 2px; }
.steps i { flex: 1; background: var(--edge); transition: background-color .5s; }
.steps i.done { background: var(--ok); }
.steps i.now { background: var(--link); }

.body { overflow-y: auto; padding-bottom: 16px; }
.group-title { margin: 0; padding: 16px 12px 4px; font-size: .85em; font-weight: 600; letter-spacing: .04em; text-transform: uppercase; color: var(--desc); }
.notice { display: flex; gap: 8px; align-items: center; padding: 16px 12px 0; color: var(--desc); }

.rows { list-style: none; padding: 0; }
.row { display: grid; grid-template-columns: 16px minmax(0, 1fr); gap: 0 8px; margin: 0; padding: 6px 12px; transition: background-color .15s; }
.row:hover, .row:focus-within { background: var(--vscode-list-hoverBackground); }
.row-title { display: block; width: 100%; border: 0; padding: 0; background: none; text-align: left; cursor: pointer; }
.row-detail { color: var(--desc); max-width: 68ch; }
.row-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 0 12px; margin-top: 2px; font-size: .92em; color: var(--desc); }
.row-actions { display: flex; align-items: center; gap: 12px; opacity: 0; transition: opacity .15s; }
.row:hover .row-actions, .row:focus-within .row-actions, .row-actions.always { opacity: 1; }
@media (hover: none) { .row-actions { opacity: 1; } }
.doing { display: inline-flex; align-items: center; gap: 6px; }
.swap { display: inline-flex; align-items: center; gap: 4px; flex-wrap: wrap; }
.swap .i { color: var(--desc); }

.turn { padding: 6px 12px 0; scroll-margin-bottom: 16px; }
.turn + .turn { margin-top: 10px; }
.q { margin-left: auto; width: fit-content; max-width: 88%; padding: 5px 10px; border-radius: 10px 10px 3px 10px; background: var(--fill); white-space: pre-wrap; }
.q-loc { text-align: right; margin-top: 2px; font-size: .92em; color: var(--desc); }
.a { max-width: 68ch; }
.q + .a, .q-loc + .a { margin-top: 8px; }
.pending { display: grid; gap: 6px; margin-top: 10px; max-width: 420px; }
.pending i { height: 9px; border-radius: 3px; background: linear-gradient(90deg, var(--fill) 25%, var(--edge) 50%, var(--fill) 75%) 0 0 / 200% 100%; animation: sheen 1.4s linear infinite; }
.pending i + i { width: 70%; }
@keyframes sheen { to { background-position: -200% 0; } }
.older { padding: 2px 12px 4px; color: var(--desc); }
.older summary { cursor: pointer; width: fit-content; max-width: 100%; }
.older summary:hover { color: var(--vscode-foreground); }
.old { margin: 4px 0 0 14px; }
.old[open] > summary { color: var(--vscode-foreground); }
.old .q-loc { text-align: left; margin: 0 0 0 14px; }
.old .a, .old .pending { margin: 4px 0 10px 14px; color: var(--vscode-foreground); }

.lesson-title { margin: 0; padding: 16px 12px 0; font-size: 1.25em; font-weight: 600; line-height: 1.3; text-wrap: balance; }
.origin { padding: 4px 12px 0; color: var(--desc); max-width: 70ch; }
.prose { padding: 0 12px; max-width: 70ch; }
.task { margin: 16px 12px 0; padding: 10px 12px 12px; border-radius: 6px; background: color-mix(in srgb, var(--link) 11%, transparent); }
.task-head { display: flex; justify-content: space-between; gap: 12px; font-weight: 600; color: var(--link); }
.task-head span { font-weight: 400; font-size: .92em; font-variant-numeric: tabular-nums; }
.task-goal { margin-top: 4px; max-width: 66ch; }
.criteria { list-style: none; margin-top: 8px; padding: 0; }
.criteria li { position: relative; padding-left: 18px; }
.criteria li::before { content: ""; position: absolute; left: 6px; top: .62em; width: 4px; height: 4px; border-radius: 50%; background: currentColor; opacity: .55; }
.task-foot { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 12px; margin-top: 12px; color: var(--desc); font-size: .92em; }
.win { position: relative; display: grid; grid-template-columns: 16px minmax(0, 1fr); gap: 0 8px; margin: 12px 12px 0; padding: 10px 12px; border-radius: 6px; background: color-mix(in srgb, var(--ok) 13%, transparent); }
.win-label { font-size: .92em; color: var(--desc); }
.win-title { font-weight: 600; }
.pop { animation: pop .55s cubic-bezier(.16, 1, .3, 1); }
@keyframes pop { 0% { transform: scale(.95); opacity: 0; } 100% { transform: scale(1); opacity: 1; } }
.confetti { position: absolute; left: 20px; top: 18px; pointer-events: none; }
.confetti i { position: absolute; width: 6px; height: 6px; border-radius: 1px; opacity: 0; animation: burst 1s ease-out forwards; }
@keyframes burst { 0% { transform: translate(0, 0) scale(.6); opacity: 1; } 100% { transform: translate(var(--x), var(--y)) rotate(220deg); opacity: 0; } }

.ask { margin: 12px 12px 0; padding: 12px; border: 1px solid var(--vscode-focusBorder); border-radius: 6px; }
.ask-q { font-weight: 600; max-width: 60ch; }
.ask-intro { margin-top: 2px; color: var(--desc); }
.ask pre { margin-top: 8px; }
.choices { display: grid; gap: 6px; margin-top: 10px; }
.choices .btn { text-align: left; white-space: normal; padding: 6px 10px; }
.ask-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 14px; margin-top: 12px; }
.perm { display: flex; align-items: center; gap: 6px; margin-bottom: 4px; font-size: .92em; color: var(--fix); }
.notions { margin-top: 10px; }
.notion { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 12px; padding: 8px 0; border-top: 1px solid var(--hair); }
.notion-text { flex: 1 1 180px; min-width: 0; }
.notion-text small { display: block; font-size: .92em; color: var(--desc); }
.tag { margin-left: 4px; padding: 0 6px; border-radius: 8px; font-size: .85em; background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); }
.seg { flex: none; display: inline-flex; border: 1px solid var(--edge); border-radius: 5px; overflow: hidden; }
.seg button { border: 0; padding: 2px 9px; background: none; font-size: .92em; cursor: pointer; }
.seg button + button { border-left: 1px solid var(--edge); }
.seg button.on { color: var(--vscode-editor-background); font-weight: 600; }
.seg button.on.known { background: var(--ok); } .seg button.on.unsure { background: var(--fix); } .seg button.on.unknown { background: var(--link); }
label.check { display: flex; gap: 8px; align-items: center; margin-top: 8px; cursor: pointer; }
textarea { display: block; width: 100%; margin-top: 10px; padding: 6px 8px; border: 1px solid var(--vscode-input-border, var(--edge)); border-radius: 4px; font: inherit; color: var(--vscode-input-foreground); background: var(--vscode-input-background); resize: vertical; }
textarea:focus { outline: 1px solid var(--vscode-focusBorder); outline-offset: -1px; }

.empty { padding: 24px 12px 0; max-width: 60ch; }
.empty h2 { margin: 0 0 4px; font-size: 1.1em; font-weight: 600; }
.empty p { color: var(--desc); }
.empty .lead-in { margin-top: 20px; color: var(--vscode-foreground); }
.empty dl { display: grid; grid-template-columns: max-content 1fr; gap: 6px 14px; margin: 8px 0 0; }
.empty dt { font-family: var(--vscode-editor-font-family); color: var(--vscode-textPreformat-foreground, var(--link)); }
.empty dd { margin: 0; color: var(--desc); }
.sheet dl { margin-top: 16px; }
.sheet-note { margin-top: 20px; font-size: .92em; }

.composer { padding: 8px 12px 12px; border-top: 1px solid var(--hair); }
.help { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 8px; }
.help:empty { display: none; }
.help .btn { padding: 2px 9px; font-size: .92em; }
.help .btn small { font-size: inherit; opacity: .65; margin-left: 2px; }
.field { display: grid; gap: 4px; padding: 6px 4px 4px 8px; border: 1px solid var(--vscode-input-border, var(--edge)); border-radius: 6px; background: var(--vscode-input-background); cursor: text; }
.field:focus-within { border-color: var(--vscode-focusBorder); }
.field textarea, .field textarea:focus { box-sizing: border-box; width: 100%; margin: 0; padding: 0 4px 0 0; border: 0; border-radius: 0; outline: 0; resize: none; overflow-y: hidden; background: none; font: inherit; color: var(--vscode-input-foreground); caret-color: var(--link); }
.field textarea::placeholder { color: var(--vscode-input-placeholderForeground); }
.field-foot { display: flex; align-items: center; gap: 8px; min-height: 24px; }
.keys { flex: 1; min-width: 0; text-align: right; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: .92em; color: var(--desc); visibility: hidden; }
.field:focus-within .keys { visibility: visible; }
.ctx { flex: none; padding: 0 6px; border-radius: 3px; background: var(--fill); font-size: .92em; color: var(--desc); font-variant-numeric: tabular-nums; }
.send { flex: none; display: grid; place-items: center; width: 24px; height: 24px; border: 0; border-radius: 4px; cursor: pointer; background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
.send:hover:enabled { background: var(--vscode-button-hoverBackground); }
`;

/** Les outils communs des pages web. Tourne dans la page web. */
function webKit() {
  const ICONS = {
    check: '<path d="M3 8.5l3 3 7-7"/>',
    alert: '<path d="M8 2l6.5 11.5h-13z"/><path d="M8 6.5v3"/><path d="M8 11.8v.2"/>',
    bulb: '<path d="M6 12.5h4M6.5 14.5h3M8 1.5a4.5 4.5 0 0 0-2.6 8.2c.4.3.6.8.6 1.3h4c0-.5.2-1 .6-1.3A4.5 4.5 0 0 0 8 1.5z"/>',
    pencil: '<path d="M3 13l.8-3.2L11 2.6l2.4 2.4-7.2 7.2z"/>',
    blocks: '<rect x="2" y="2" width="5" height="5" rx=".5"/><rect x="9" y="2" width="5" height="5" rx=".5"/><rect x="2" y="9" width="5" height="5" rx=".5"/><path d="M11.5 9.5v4M9.5 11.5h4"/>',
    arrow: '<path d="M3 8h9"/><path d="M9 5l3 3-3 3"/>',
    send: '<path d="M8 13V3.5"/><path d="M4 7l4-4 4 4"/>',
    marks: '<path d="M7 3L4 13M12 3L9 13"/>',
  };
  const DOTS =
    '<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><circle cx="3.5" cy="8" r="1.2" fill="currentColor"/><circle cx="8" cy="8" r="1.2" fill="currentColor"/><circle cx="12.5" cy="8" r="1.2" fill="currentColor"/></svg>';
  const TONE_ICONS = { ok: 'check', fix: 'alert', design: 'blocks', info: 'bulb', assist: 'pencil' };
  /** Ce qui casse d'abord, les compliments en dernier. */
  const TONE_RANK = { fix: 0, design: 1, info: 2, ok: 3 };
  const esc = (s) => String(s == null ? '' : s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
  const icon = (name, label) =>
    `<svg class="i" viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" ${label ? `role="img" aria-label="${esc(label)}"` : 'aria-hidden="true"'}>${ICONS[name] || ''}</svg>`;
  const tone = (name, t) => `<span class="tone ${name}">${icon(TONE_ICONS[name], (t.tones || {})[name])}</span>`;
  const spinner = () => '<span class="spin" aria-hidden="true"></span>';
  const tick = String.fromCharCode(96);
  const inline = (s) =>
    esc(s)
      .replace(new RegExp(tick + '([^' + tick + ']+)' + tick, 'g'), '<code>$1</code>')
      .replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>')
      .replace(/(^|[^*\w])\*([^*\s][^*]*)\*/g, '$1<i>$2</i>');
  const fence = tick + tick + tick;
  function md(text) {
    const lines = String(text || '').split('\n');
    let html = '';
    let list = false;
    let code = null;
    let para = [];
    const flush = () => {
      if (para.length) html += '<p>' + inline(para.join(' ')) + '</p>';
      para = [];
    };
    const endList = () => {
      if (list) html += '</ul>';
      list = false;
    };
    for (const line of lines) {
      if (code !== null) {
        if (line.trim().startsWith(fence)) {
          html += '<pre><code>' + esc(code.join('\n')) + '</code></pre>';
          code = null;
        } else code.push(line);
        continue;
      }
      if (line.trim().startsWith(fence)) {
        flush();
        endList();
        code = [];
        continue;
      }
      const item = line.match(/^\s*[-*]\s+(.*)$/);
      if (item) {
        flush();
        if (!list) html += '<ul>';
        list = true;
        html += '<li>' + inline(item[1]) + '</li>';
        continue;
      }
      if (!line.trim()) {
        flush();
        endList();
        continue;
      }
      endList();
      para.push(line.trim().replace(/^#+\s+/, ''));
    }
    if (code !== null) html += '<pre><code>' + esc(code.join('\n')) + '</code></pre>';
    flush();
    endList();
    return html;
  }
  const base = (f) => String(f || '').split(/[\\/]/).pop();
  const at = (file, line) => `${esc(base(file))}:${line + 1}`;
  /** Un passage de code sur une ligne, raccourci. */
  const short = (x) => {
    const flat = String(x || '').split('\n').map((l) => l.trim()).filter(Boolean).join(' ⏎ ');
    return flat.length > 70 ? flat.slice(0, 69) + '…' : flat;
  };

  return { ICONS, DOTS, TONE_ICONS, TONE_RANK, esc, icon, tone, spinner, inline, md, base, at, short };
}

/** Le script du panneau. Il tourne dans la page web : il ne voit rien d'autre de ce fichier. */
function panelMain() {
  const vscode = acquireVsCodeApi();
  let S = null;
  let CTX = null;
  let menu = false;
  // L'aide-mémoire des commentaires, à la place de la discussion.
  let sheet = false;
  let sheetScroll = 0;
  let askedId = null;
  let olderOpen = false;
  const openOld = new Set();
  let choices = {};
  let multi = {};
  let promptId = null;
  let answersSeen = null;
  let pop = { seq: null, at: 0 };
  const shown = {};
  const $ = (id) => document.getElementById(id);
  /** Ne réécrit une zone que si son contenu change : le survol, le focus et ce qui est déplié tiennent. */
  const put = (id, html) => {
    if (shown[id] === html) return false;
    shown[id] = html;
    $(id).innerHTML = html;
    return true;
  };

  /** Les modes sans étape, où vibelearn regarde chaque sauvegarde. */
  const watchMode = () => S.mode === 'monitor' || S.mode === 'assist';
  const { DOTS, esc, icon, tone, spinner, inline, md, base, at, short } = KIT;

  function confetti() {
    const colors = ['#378ADD', '#1D9E75', '#EF9F27', '#D4537E', '#7F77DD'];
    let s = '<div class="confetti" aria-hidden="true">';
    for (let i = 0; i < 18; i++) {
      const a = (i / 18) * Math.PI * 2;
      const d = 36 + (i % 4) * 14;
      s += `<i style="--x:${Math.round(Math.cos(a) * d)}px;--y:${Math.round(Math.sin(a) * d - 18)}px;background:${colors[i % colors.length]};animation-delay:${(i % 3) * 40}ms"></i>`;
    }
    return s + '</div>';
  }

  /** Une question de vibelearn : le seul encadré du panneau, parce qu'il attend une réponse. */
  function promptCard(p, t) {
    if (p.id !== promptId) {
      promptId = p.id;
      choices = {};
      multi = {};
      if (p.kind === 'checklist') p.items.forEach((i) => (choices[i.id] = i.state));
    }
    const code = p.code ? `<pre><code>${esc(p.code)}</code></pre>` : '';
    const dontKnow = `<button class="link" type="button" data-choice="-1">${esc(t.dont_know)}</button>`;
    let h = '<div class="ask">';
    if (p.kind === 'checklist') {
      h += `<p class="ask-q">${esc(p.title)}</p><p class="ask-intro">${inline(p.intro || t.checklist_hint)}</p><div class="notions">`;
      h += p.items
        .map(
          (i) =>
            `<div class="notion"><div class="notion-text">${inline(i.label)}${i.kind === 'bonus' ? '<span class="tag">bonus</span>' : ''}` +
            (i.why ? `<small>${inline(i.why)}</small>` : '') +
            (i.evidence ? `<small>${inline(i.evidence)}</small>` : '') +
            '</div><div class="seg">' +
            ['known', 'unsure', 'unknown']
              .map((v) => `<button type="button" data-seg="${esc(i.id)}" data-v="${v}" aria-pressed="${choices[i.id] === v}" class="${choices[i.id] === v ? 'on ' + v : ''}">${esc(t['claim_' + v])}</button>`)
              .join('') +
            '</div></div>',
        )
        .join('');
      h += `</div><div class="ask-row"><button class="btn primary" type="button" data-go="1">${esc(t.go)}</button></div>`;
    } else if (p.kind === 'choice' || p.kind === 'confidence') {
      const opts = p.kind === 'confidence' ? ['1', '2', '3', '4', '5'] : p.options;
      h += `<p class="ask-q">${inline(p.question)}</p>${code}<div class="choices">`;
      h += opts.map((o, i) => `<button class="btn" type="button" data-choice="${i}">${inline(o)}</button>`).join('') + '</div>';
      if (p.kind === 'choice') h += `<div class="ask-row">${dontKnow}</div>`;
    } else if (p.kind === 'multi') {
      h += `<p class="ask-q">${inline(p.question)}</p>`;
      h += p.options.map((o, i) => `<label class="check"><input type="checkbox" data-multi="${i}" ${multi[i] ? 'checked' : ''}> ${inline(o)}</label>`).join('');
      h += `<div class="ask-row"><button class="btn primary" type="button" data-multigo="1">${esc(t.send)}</button>${dontKnow}</div>`;
    } else if (p.kind === 'free' || p.kind === 'predict') {
      h += `<p class="ask-q">${inline(p.question)}</p>${code}`;
      h += `<textarea id="free" rows="3" aria-label="${esc(p.question)}"></textarea><div class="ask-row"><button class="btn primary" type="button" data-free="1">${esc(t.send)}</button>${dontKnow}</div>`;
    } else if (p.kind === 'permission') {
      h += `<p class="perm">${icon('alert')}${esc(t.permission)}</p><p class="ask-q">${esc(p.headline)}</p>`;
      h += p.detail ? `<pre><code>${esc(p.detail)}</code></pre>` : '';
      h += p.write ? `<p class="ask-intro">${esc(t.write_warning)}</p>` : '';
      h += `<div class="ask-row"><button class="btn primary" type="button" data-perm="allow">${esc(t.allow)}</button><button class="btn" type="button" data-perm="always">${esc(t.always)}</button><button class="link" type="button" data-perm="deny">${esc(t.deny)}</button></div>`;
    }
    return h + '</div>';
  }

  /** Ce que vibelearn a corrigé : une liste, la ligne au clic. Ses remarques sont dans la fenêtre flottante de vibelearn. */
  function rowsHtml(t, ended) {
    const edits = ended ? [] : S.edits || [];
    if (!edits.length) return '';
    let h = '<ul class="rows">';
    for (const e of edits) {
      const act = e.status === 'applied' ? `<button class="link" type="button" data-undo="${e.id}">${esc(t.undo)}</button>` : `<span class="doing">${spinner()}${esc(t.applying)}</span>`;
      h += `<li class="row">${tone('assist', t)}<div><button class="row-title" type="button" data-gotoe="${e.id}"><span class="swap"><code>${esc(short(e.old))}</code>${icon('arrow', t.becomes)}<code>${esc(short(e.new))}</code></span></button>`;
      h += `<div class="row-detail">${inline(e.reason)}</div><p class="row-meta"><button class="loc" type="button" data-gotoe="${e.id}">${at(e.file, e.line)}</button><span class="row-actions always">${act}</span></p></div></li>`;
    }
    return h + '</ul>';
  }

  /** La discussion, dans l'ordre : la dernière question en entier, les précédentes repliées. */
  function talkHtml(t) {
    const asked = (S.talk || []).map((q) => ({ id: q.id, q }));
    const items = (S.notes || []).map((n) => ({ id: n.id, note: n.text })).concat(asked).sort((a, b) => a.id - b.id);
    if (!items.length) return '';
    const latest = asked.length ? asked[asked.length - 1].q : null;
    const old = items.filter((it) => it.q && it.q !== latest && it.q.answers.length);
    const where = (q) => (q.file ? `<p class="q-loc"><button class="loc" type="button" data-gotoq="${q.id}">${at(q.file, q.line)}</button></p>` : '');
    const answers = (q) => (q.answers.length ? `<div class="a md">${q.answers.map(md).join('')}</div>` : '<div class="pending" aria-hidden="true"><i></i><i></i></div>');
    let h = '';
    if (old.length) {
      h += `<details class="older" data-older="1"${olderOpen ? ' open' : ''}><summary>${old.length} ${esc(old.length > 1 ? t.older : t.older_one)}</summary>`;
      h += old.map(({ q }) => `<details class="old" data-old="${q.id}"${openOld.has(String(q.id)) ? ' open' : ''}><summary>${inline(q.question)}</summary>${where(q)}${answers(q)}</details>`).join('');
      h += '</details>';
    }
    for (const it of items) {
      if (old.includes(it)) continue;
      h += it.q ? `<article class="turn"><p class="q">${inline(it.q.question)}</p>${where(it.q)}${answers(it.q)}</article>` : `<article class="turn"><div class="a md">${md(it.note)}</div></article>`;
    }
    return h;
  }

  function render() {
    if (!S) return;
    const t = S.strings || {};
    const ended = S.status === 'ended';
    // Une question de vibelearn passe devant l'aide-mémoire.
    if (S.prompt && S.prompt.id !== askedId) sheet = false;
    askedId = S.prompt ? S.prompt.id : null;
    // Sous le titre d'une leçon, les groupes descendent d'un niveau.
    const H = S.step && !ended ? 'h3' : 'h2';
    const group = (label) => `<${H} class="group-title">${esc(label)}</${H}>`;

    // Ce qui est en train d'être tapé survit au rafraîchissement.
    const free = $('free');
    const keep = free ? { value: free.value, focus: document.activeElement === free } : null;

    let h = '';
    if (ended) h += `<p class="notice">${esc(t.ended)}</p>`;
    if (S.prompt) h += promptCard(S.prompt, t);
    const c = S.celebrate;
    if (c && !ended) {
      // Les confettis jouent une fois, sans être coupés par un rafraîchissement.
      if (c.seq !== pop.seq) pop = { seq: c.seq, at: Date.now() };
      const fresh = Date.now() - pop.at < 1200;
      h += `<div class="win${fresh ? ' pop' : ''}">${fresh ? confetti() : ''}${tone('ok', {})}<div><p class="win-label">${esc(t.done)}</p><p class="win-title">${esc(c.title)}</p>${c.summary ? `<p>${inline(c.summary)}</p>` : ''}</div></div>`;
    }
    if (S.step && !ended) {
      const s = S.step;
      h += `<h2 class="lesson-title">${esc(s.title)}</h2>`;
      // Mini-leçon : la remarque qu'elle approfondit.
      if (s.origin) h += `<p class="origin">${inline(s.origin)}</p>`;
      if (s.notes) h += `<h3 class="group-title">${esc(t.know)}</h3><div class="prose md">${md(s.notes)}</div>`;
      const where = s.file ? `<span>${esc(base(s.file))}${s.line != null ? ':' + (s.line + 1) : ''}</span>` : '';
      h += `<div class="task"><p class="task-head">${esc(t.your_turn)}${where}</p><div class="task-goal md">${md(s.goal)}</div>`;
      if (s.criteria && s.criteria.length) h += `<ul class="criteria">${s.criteria.map((x) => `<li>${inline(x)}</li>`).join('')}</ul>`;
      if (s.file) h += `<p class="task-foot"><button class="btn" type="button" data-where="1">${esc(t.where)}</button><span>${esc(t.watching)}</span></p>`;
      h += '</div>';
    } else if (!watchMode() && !S.prompt && !ended && !c) {
      h += `<p class="notice">${spinner()}<span>${esc(t.reading)}</span></p>`;
    }
    const rows = rowsHtml(t, ended);
    if (rows) h += group(t.on_code) + rows;
    const talk = ended ? '' : talkHtml(t);
    if (talk) h += group(t.talk) + talk;
    // Rien à signaler : le panneau rappelle comment s'en servir.
    const marks = `<dl>${(t.marks || []).map(([mark, what]) => `<dt>${esc(mark)}</dt><dd>${esc(what)}</dd>`).join('')}</dl>`;
    if (!h && watchMode()) h = `<div class="empty"><h2>${esc(t.watch_title)}</h2><p>${esc(S.mode === 'assist' ? t.assist_text : t.watch_text)}</p><p class="lead-in">${esc(t.marks_intro)}</p>${marks}</div>`;
    // L'aide-mémoire, à la demande : il prend la place de tout le reste.
    if (sheet && !ended) h = `<div class="empty sheet"><h2>${esc(t.marks_title)}</h2><p>${esc(t.marks_hint)}</p>${marks}<p class="sheet-note">${inline(t.marks_note)}</p></div>`;
    if (put('main', h) && keep && $('free')) {
      $('free').value = keep.value;
      if (keep.focus) $('free').focus();
    }
    // Une nouvelle réponse : on la montre (pas celles déjà là quand le panneau s'ouvre).
    const answers = (S.talk || []).reduce((n, q) => n + q.answers.length, 0);
    if (answersSeen !== null && answers > answersSeen) {
      const turns = document.querySelectorAll('.turn');
      if (turns.length) turns[turns.length - 1].scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    }
    answersSeen = answers;
    renderBar();
  }

  /** L'état et les commandes : « Vérifier » sous la main, les aides au-dessus du champ, le reste derrière « ⋯ ». */
  function renderBar() {
    const t = S.strings || {};
    const ended = S.status === 'ended';
    const busy = S.status === 'busy' || S.status === 'starting';
    const acts = S.actions || [];
    const place = (p) => acts.filter((a) => a.place === p);
    const off = ended ? ' disabled' : '';
    const steps = (!watchMode() && S.plan && S.plan.steps) || [];
    const active = steps.findIndex((x) => x.status === 'active');
    const done = steps.filter((x) => x.status === 'done' || x.status === 'skipped').length;
    const cur = active >= 0 ? active + 1 : Math.min(done + 1, steps.length);
    const cap = (x) => String(x || '').replace(/^./, (ch) => ch.toUpperCase());
    const title = watchMode() ? cap(S.step ? t.lesson : S.mode === 'assist' ? t.assist : t.monitor) : steps.length ? `${t.step} ${cur} ${t.of} ${steps.length}` : 'vibelearn';
    const status = S.prompt && !busy && !ended ? t.waiting : S.status_text;

    let h = `<span class="state${ended ? ' off' : busy ? ' busy' : S.prompt ? ' wait' : ''}" aria-hidden="true"></span>`;
    h += `<p class="bar-text" title="${esc(status)}"><strong>${esc(title)}</strong><span>${esc(status)}</span></p>`;
    h += place('bar').map((a) => `<button class="btn" type="button" data-cmd="${esc(a.name)}"${off}>${esc(a.label)}</button>`).join('');
    if (!ended) h += `<button class="icon-btn" type="button" data-sheet="1" aria-pressed="${sheet}" aria-label="${esc(t.marks_show)}" title="${esc(t.marks_show)}">${icon('marks')}</button>`;
    const items = place('menu');
    // Le modèle qui répond (connu une fois la session démarrée), et l'effort demandé.
    const info = S.model ? `${S.model}${S.effort ? `, ${t.effort || 'effort'} ${S.effort}` : ''}` : '';
    if (items.length || info) {
      h += `<button class="icon-btn" type="button" data-menu="1" aria-label="${esc(t.more_actions)}" aria-expanded="${menu}">${DOTS}</button><div class="menu"${menu ? '' : ' hidden'}>`;
      h += items.map((a, i) => (a.name === 'quit' && i ? '<hr>' : '') + `<button class="menu-item" type="button" data-cmd="${esc(a.name)}"${off}>${esc(a.label)}</button>`).join('');
      h += (info ? (items.length ? '<hr>' : '') + `<p class="menu-note">${esc(info)}</p>` : '') + '</div>';
    }
    if (steps.length) {
      h += `<div class="steps" role="progressbar" aria-label="${esc(t.step)}" aria-valuemin="0" aria-valuemax="${steps.length}" aria-valuenow="${cur}">`;
      h += steps.map((st) => `<i class="${st.status === 'active' ? 'now' : st.status === 'pending' ? '' : 'done'}" title="${esc(st.title)}"></i>`).join('') + '</div>';
    }
    put('bar', h);

    const level = S.step ? ` <small>${S.step.hint_level}/4</small>` : '';
    put('help', place('help').map((a) => `<button class="btn" type="button" data-cmd="${esc(a.name)}"${off}>${esc(a.label)}${a.name === 'hint' ? level : ''}</button>`).join(''));
    renderAsk();
  }

  function renderAsk() {
    if (!S) return;
    const t = S.strings || {};
    const ended = S.status === 'ended';
    $('ask').placeholder = t.ask_placeholder || '';
    $('ask').setAttribute('aria-label', t.ask_placeholder || '');
    $('ask').disabled = ended;
    $('send').disabled = ended;
    $('send').setAttribute('aria-label', t.send || '');
    $('send').title = t.send || '';
    $('keys').textContent = ended ? '' : t.ask_keys || '';
    if (!$('send').firstChild) $('send').innerHTML = icon('send');
    // La ligne du curseur : la question portera dessus.
    $('ctx').hidden = !CTX;
    if (CTX) {
      $('ctx').textContent = `${t.line} ${CTX.line}`;
      $('ctx').title = `${t.ask_about} ${CTX.line} (${CTX.file})`;
    }
  }

  /** Le champ grandit avec ce qu'on écrit, jusqu'à 40 % du panneau ; au-delà, il défile. */
  function growAsk() {
    const a = $('ask');
    a.style.height = 'auto';
    const full = a.scrollHeight;
    // Panneau caché : rien à mesurer.
    if (!full) return;
    const max = Math.max(72, Math.round(window.innerHeight * 0.4));
    a.style.height = Math.min(full, max) + 'px';
    a.style.overflowY = full > max ? 'auto' : 'hidden';
  }

  /** Montre ou range l'aide-mémoire ; la discussion revient où on l'avait laissée. */
  function toggleSheet() {
    const main = $('main');
    if (!sheet) sheetScroll = main.scrollTop;
    sheet = !sheet;
    render();
    main.scrollTop = sheet ? 0 : sheetScroll;
    const button = document.querySelector('[data-sheet]');
    if (button) button.focus();
  }

  function answer(a) {
    if (S && S.prompt) vscode.postMessage({ type: 'answer', prompt: S.prompt.id, answer: a });
  }

  document.addEventListener('click', (e) => {
    if (!S) return;
    const el = e.target.closest('button');
    // Le menu : son bouton l'ouvre et le ferme, un clic ailleurs le ferme.
    const wasOpen = menu;
    if (menu && !e.target.closest('.menu')) menu = false;
    if (el && el.dataset.menu) menu = !wasOpen;
    if (!el) {
      if (menu !== wasOpen) renderBar();
      return;
    }
    const d = el.dataset;
    if (d.cmd) {
      menu = false;
      vscode.postMessage({ type: 'command', name: d.cmd });
    } else if (d.sheet) toggleSheet();
    else if (d.undo) vscode.postMessage({ type: 'undo', id: Number(d.undo) });
    else if (d.gotoe) {
      const x = (S.edits || []).find((y) => String(y.id) === d.gotoe);
      if (x) vscode.postMessage({ type: 'goto', file: x.file, line: x.line, anchor: x.new });
    } else if (d.where) vscode.postMessage({ type: 'where' });
    else if (d.gotoq) {
      const q = (S.talk || []).find((x) => String(x.id) === d.gotoq);
      if (q && q.file) vscode.postMessage({ type: 'goto', file: q.file, line: q.line });
    } else if (d.seg) {
      choices[d.seg] = d.v;
      render();
    } else if (d.go) {
      const known = Object.keys(choices).filter((k) => choices[k] === 'known');
      const unsure = Object.keys(choices).filter((k) => choices[k] === 'unsure');
      answer({ known, unsure });
    } else if (d.choice !== undefined) {
      const i = Number(d.choice);
      answer(i < 0 ? { dont_know: true } : S.prompt.kind === 'confidence' ? { confidence: i + 1 } : { choice: i });
    } else if (d.multigo) {
      const list = Object.keys(multi).filter((k) => multi[k]).map(Number);
      if (list.length) answer({ choices: list });
    } else if (d.free) {
      const v = $('free').value.trim();
      if (v) answer({ text: v });
    } else if (d.perm) answer({ decision: d.perm });
    if (menu === wasOpen) return;
    renderBar();
    // Au clavier : le focus entre dans le menu, et revient à son bouton quand il le referme.
    const next = menu ? document.querySelector('.menu-item:enabled') : d.menu ? document.querySelector('[data-menu]') : null;
    if (next) next.focus();
  });
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    // Échap ferme le menu, sinon l'aide-mémoire.
    if (menu) {
      menu = false;
      renderBar();
      const opener = document.querySelector('[data-menu]');
      if (opener) opener.focus();
    } else if (sheet) toggleSheet();
  });
  document.addEventListener('change', (e) => {
    const d = e.target.dataset || {};
    if (d.multi !== undefined) multi[d.multi] = e.target.checked;
  });
  // Ce qui a été déplié le reste quand la discussion se redessine.
  document.addEventListener(
    'toggle',
    (e) => {
      const d = e.target.dataset || {};
      if (d.older) olderOpen = e.target.open;
      else if (d.old) e.target.open ? openOld.add(d.old) : openOld.delete(d.old);
    },
    true,
  );
  $('askform').addEventListener('submit', (e) => {
    e.preventDefault();
    const v = $('ask').value.trim();
    if (!v || !S || S.status === 'ended') return;
    vscode.postMessage({ type: 'ask', text: v });
    $('ask').value = '';
    growAsk();
  });
  // Entrée envoie, Maj+Entrée va à la ligne (pas pendant la saisie d'un caractère composé).
  $('ask').addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return;
    e.preventDefault();
    $('askform').requestSubmit();
  });
  $('ask').addEventListener('input', growAsk);
  window.addEventListener('resize', growAsk);
  // Un clic dans le cadre, hors du texte, donne quand même la main au champ.
  $('askform').addEventListener('mousedown', (e) => {
    if (e.target.closest('textarea, button') || $('ask').disabled) return;
    e.preventDefault();
    $('ask').focus();
  });
  window.addEventListener('message', (e) => {
    const m = e.data || {};
    if (m.type === 'state') {
      S = m.state;
      CTX = m.context;
      render();
    } else if (m.type === 'context') {
      CTX = m.context;
      renderAsk();
    } else if (m.type === 'focusAsk') {
      CTX = m.context || CTX;
      renderAsk();
      $('ask').focus();
    }
  });
  vscode.postMessage({ type: 'ready' });
}

module.exports = { activate, deactivate };
