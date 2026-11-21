# Demo video: script

A one-minute screen recording for the launch posts and the README ([on YouTube](https://youtu.be/oXssE5qTpYA)). One agent reads two Wikipedia
articles and records them. A second agent, in a separate session that never saw the articles,
answers a question from the graph and quotes the sentences. The viewer then shows the source
behind a link.

The message: **memory for agents where every fact cites its source, shared between agents.**

The story is Hedy Lamarr, a 1940s film star who co-invented frequency hopping with the composer
George Antheil. The question "How did a Hollywood film star end up in the National Inventors Hall
of Fame?" has a surprising answer, and the answer takes facts from both articles.

## Setup (once per take)

```powershell
./demo/setup.ps1 -ModelDir <folder of multilingual-e5-small>
```

The script starts a throwaway server on `127.0.0.1:7499` in a scratch folder with an empty `demo`
graph. It has two agent keys, `alice` and `bob`, and prints the command for each terminal. Your
real server and graphs are not involved. `-Reset` starts over with an empty graph; `-Stop`
removes everything. Without `-ModelDir`, search matches words only, which this demo doesn't need.

Screen:

- 1920×1080, with the taskbar and notifications hidden (Focus on, or Do Not Disturb).
- Windows Terminal split vertically:
  - left pane titled **Agent 1 · alice**;
  - right pane titled **Agent 2 · bob**;
  - font size about 18 and a dark theme.
- A browser window with the viewer open at `http://127.0.0.1:7499/admin/ui/graph?graph=demo`:
  - paste the key from `viewer-key.txt` once, before recording;
  - use a clean profile with no bookmarks bar and no other tabs;
  - set the zoom to 125%.
- Start both `claude` sessions before recording, so the welcome banner is already off screen.
  The banner shows the folder; the scratch folder's path contains your Windows user name.
  Pass `-Dir C:\mygraph-demo` to the script if you'd rather it said that.

Privacy:

- `--strict-mcp-config` gives each session only the demo server: no other MCP servers, plugins'
  servers included. Your real graphs can't appear on screen.
- `--settings settings.json` turns off hooks and the custom status line for these sessions.
- Before publishing, scrub the first and last frames for the account name, email or plan.

## Shots

| Time | Screen | Caption |
|---|---|---|
| 0:00–0:03 | Title card | **MyGraph** · memory for AI agents · every fact cites its source |
| 0:03–0:20 | Left pane: type prompt 1, then the run sped up (about 18×) | Agent 1 reads two Wikipedia articles and records what they say |
| 0:20–0:23 | Left pane: the agent's summary | Each node and link it wrote cites the sentence it came from |
| 0:23–0:45 | Right pane: type prompt 2, then the answer | Agent 2, a separate session that never saw the articles, asks the graph |
| 0:45–0:55 | Browser: the viewer, select the Hedy Lamarr → National Inventors Hall of Fame link | Every link shows the sentence it was read from |
| 0:55–0:59 | End card | github.com/dieron/mygraph |

### Prompt 1 (left, alice)

```text
Read the Wikipedia articles on Hedy Lamarr and George Antheil and record what they say in MyGraph.
```

Expect:

- `graphs` and `overview`, then the articles are fetched;
- `resolve`, then one `write` per article;
- a summary of what it recorded (about 50 nodes and 60 links in the recorded take), with the gaps
  it left, such as text that came back only as a summary.

It takes about 3 minutes; speed it up and say so on screen.

### Prompt 2 (right, bob)

```text
How did a Hollywood film star end up in the National Inventors Hall of Fame? Answer only from MyGraph and quote the sources.
```

This is the question used in `mygraph-demo.mp4`. The first idea was "What does Bluetooth have to do
with a 1940s film star?", but WebFetch returns the middle of a long article only as a summary. The
agent rightly refused to cite that summary, so the Bluetooth sentence never reached the graph. The
Hall of Fame question is answered from sentences the agent got word for word.

Expect:

- `search` and `get` on Hedy Lamarr and the invention, in about 15 seconds;
- an answer in steps (the invention in 1941, the patent in 1942, first use in 1962, the Hall of
  Fame in 2014), each quoting its sentence with the article and the evidence ID, for example
  "This work led to their induction into the National Inventors Hall of Fame in 2014.";
- possibly a caveat that Wikipedia marks the invention section of the Antheil article as disputed.

If the answer runs long, crop to the quotes rather than speeding it up: people need to be able to
read them.

### Viewer (browser)

1. Reload the page so the graph appears, coloured by type, with the legend.
2. Type `Hedy Lamarr` in the search box and pick Hedy Lamarr. The panel lists her links.
3. Click the **Hedy Lamarr → National Inventors Hall of Fame** link. The panel shows the
   evidence: the sentence from the George Antheil article and its URL. Hold for 2–3 seconds.

## Recording and editing

- **Recording:** OBS, or the Xbox Game Bar (Win+Alt+R) for the whole screen. Record each
  shot separately; it's easier to retake one.
- **Editing:** Clipchamp, which comes with Windows, is enough:
  - speed up the agent's tool calls;
  - add a small "⏩ 10×" badge on sped-up parts, so it doesn't look faked;
  - burn in the captions. Most feeds autoplay without sound, so leave the video silent.
- **Exports:**
  - MP4 at 1080p for X, LinkedIn and the launch posts;
  - a GIF for the README, under 10 MB: 960 px wide, 12 fps, made with ScreenToGif or
    `ffmpeg -i demo.mp4 -vf "fps=12,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse" demo.gif`.
- **Before each take:** `./demo/setup.ps1 -Reset ...` empties the graph. Then start both
  sessions again, because each agent caches what it read.

## Optional extra beats

Add these only if the video still fits in 60 seconds.

- **Another language:** ask agent 2 in Ukrainian (`Яка кіноактриса була винахідницею?`).
  This needs `-ModelDir`. In the dry run:
  - searching the keywords `кіноактриса винахідниця` found Hedy Lamarr by meaning;
  - searching the whole question found nothing.

  So it depends on how the agent searches; try it before planning a shot around it.
- **Corrections:** agent 2 retracts a fact, and `history` shows who did what. This is better
  saved for a second video about corrections.
- **Phone:** ask the same question from claude.ai on the phone. It needs a server reachable over
  OAuth, with a key limited to a demo graph, so that your own graphs can't appear. Leave it for
  later.
