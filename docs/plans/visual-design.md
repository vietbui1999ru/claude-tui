# Claude TUI - Visual Design Specification

**Date:** 2026-03-05
**Author:** Designer Agent
**Status:** Complete

---

## 1. TUI Dashboard - Main Layout

### Overall Frame

The terminal is divided into four horizontal bands: header, tab bar, content area, and footer.
The content area is split vertically: 70% main view, 30% sidebar.

**Minimum terminal size:** 100 columns x 30 rows
**Recommended:** 120 columns x 40 rows

```
+==============================================================================================+
| Claude Usage Monitor                                          [Connected]  2026-03-05 14:32  |
+==============================================================================================+
| [Tokens]  [Costs]  [Models]  [Live]                                                         |
+----------------------------------------------------------------------+-----------------------+
|                                                                      |  Today's Summary      |
|                                                                      |                       |
|                         Main Content Area                            |  Model: Sonnet        |
|                            (70% width)                               |  Sessions: 12         |
|                                                                      |  Tokens: 145.2K       |
|                                                                      |  Cost: $3.47          |
|                                                                      |                       |
|                                                                      |  Budget (Daily)       |
|                                                                      |  ████████░░░░ 67%     |
|                                                                      |  $3.47 / $5.00        |
|                                                                      |                       |
|                                                                      |  Budget (Monthly)     |
|                                                                      |  ██░░░░░░░░░░ 15%     |
|                                                                      |  $22.30 / $150.00     |
|                                                                      |                       |
|                                                                      |  Top Model Today      |
|                                                                      |  Sonnet  84%          |
|                                                                      |  Opus    12%          |
|                                                                      |  Haiku    4%          |
|                                                                      |                       |
+----------------------------------------------------------------------+-----------------------+
| q: Quit  r: Refresh  <-/->: Tabs  Up/Down: Scroll  ?: Help                                 |
+==============================================================================================+
```

### Layout Proportions

| Section       | Constraint                          |
|---------------|-------------------------------------|
| Header        | `Constraint::Length(1)`             |
| Tab Bar       | `Constraint::Length(1)`             |
| Content Area  | `Constraint::Min(20)`              |
| Footer        | `Constraint::Length(1)`             |

Content area horizontal split:

| Section       | Constraint                          |
|---------------|-------------------------------------|
| Main View     | `Constraint::Percentage(70)`        |
| Sidebar       | `Constraint::Percentage(30)`        |

### Header Detail

```
 Claude Usage Monitor                                          [Connected]  2026-03-05 14:32
```

- Left-aligned: App title in bold primary color
- Right-aligned: Connection status indicator + datetime
- Connection states:
  - `[Connected]` - green text
  - `[Reconnecting...]` - yellow text, blinking if supported
  - `[Disconnected]` - red text

### Tab Bar Detail

```
 [Tokens]  [Costs]  [Models]  [Live]
```

- Active tab: bold text with primary color background, white foreground
- Inactive tabs: dim text, default background
- Tabs are rendered as a `Tabs` widget with `Divider::from(" ")`

### Footer Detail

```
 q: Quit  r: Refresh  <-/->: Tabs  Up/Down: Scroll  ?: Help
```

- Key hints in bold, descriptions in dim
- Layout: evenly spaced across the width

---

## 2. Tab View Mockups

### 2.1 Tokens Tab

```
+----------------------------------------------------------------------+-----------------------+
|  Token Usage                                            [7d] 30d 90d |  Today's Summary      |
|                                                                      |                       |
|  250K |                                                              |  Model: Sonnet        |
|       |          ##                                                  |  Sessions: 12         |
|  200K |          ##                                                  |  Tokens: 145.2K       |
|       |          ## ++                                               |  Cost: $3.47          |
|  150K |    ##    ## ++          ##                                    |                       |
|       |    ## ++ ## ++ ##       ##                                    |  Budget (Daily)       |
|  100K |    ## ++ ## ++ ## ++    ## ++                                 |  ████████░░░░ 67%     |
|       | ## ## ++ ## ++ ## ++ ## ## ++    ##                            |  $3.47 / $5.00        |
|   50K | ## ## ++ ## ++ ## ++ ## ## ++ ## ## ++                         |                       |
|       | ## ## ++ ## ++ ## ++ ## ## ++ ## ## ++                         |  Budget (Monthly)     |
|     0 +------------------------------------------                    |  ██░░░░░░░░░░ 15%     |
|        Feb  Feb  Mar  Mar  Mar  Mar  Mar                             |  $22.30 / $150.00     |
|         27   28   01   02   03   04   05                             |                       |
|                                                                      |  Top Model Today      |
|  Legend: ## Input  ++ Output  .. Cache Read                          |  Sonnet  84%          |
|                                                                      |  Opus    12%          |
+----------------------------------------------------------------------+  Haiku    4%          |
```

**Chart behavior:**
- Bar chart with stacked bars: input (bottom), output (middle), cache (top)
- X-axis: dates, auto-scaled to fit width
- Y-axis: token count with K/M suffixes
- Time range selector: `[7d]` active by default, toggle with `1`, `2`, `3` keys
- Active range shown in brackets with bold text
- Bars rendered using ratatui `BarChart` widget with `BarGroup` per date

### 2.2 Costs Tab

```
+----------------------------------------------------------------------+-----------------------+
|  Cost Breakdown                                                      |  Today's Summary      |
|                                                                      |                       |
|  By Model (Last 30d)                  Cumulative Spend               |  Model: Sonnet        |
|  +-----------------------+            $150 |              ....**     |  Sessions: 12         |
|  | Opus    ████████  $67.20 (52%)     |    |          ....*          |  Tokens: 145.2K       |
|  | Sonnet  █████     $41.50 (32%)     |    |      ...*               |  Cost: $3.47          |
|  | Haiku   ██        $20.30 (16%)     | $75|  ..*                    |                       |
|  +-----------------------+            |    |.*                       |  Budget (Daily)       |
|                                       |  $0+------------------      |  ████████░░░░ 67%     |
|  By Project (Last 30d)                     Feb         Mar           |  $3.47 / $5.00        |
|  +--------------------------------------------+                     |                       |
|  | Project          | Model  | Cost   | Pct  |                      |  Budget (Monthly)     |
|  |--------------------------------------------|                     |  ██░░░░░░░░░░ 15%     |
|  | claude-tui       | Sonnet | $18.40 |  14% |                      |  $22.30 / $150.00     |
|  | ml-pipeline      | Opus   | $45.20 |  35% |                      |                       |
|  | docs-generator   | Sonnet | $12.80 |  10% |                      |  Top Model Today      |
|  | code-review      | Haiku  |  $8.50 |   7% |                      |  Sonnet  84%          |
|  | (other)          | Mixed  | $44.10 |  34% |                      |  Opus    12%          |
|  +--------------------------------------------+                     |  Haiku    4%          |
|                                                                      |                       |
|  Budget: ████████████████░░░░░░░░ 67%  $129.00 / $150.00   [!] 75% |                       |
+----------------------------------------------------------------------+-----------------------+
```

**Layout within Costs tab:**
- Top-left: Model cost breakdown as horizontal bar chart
- Top-right: Cumulative spend sparkline/line chart
- Middle: Project cost table (scrollable, sorted by cost descending)
- Bottom: Full-width budget progress bar with threshold marker `[!]`
- Budget bar color shifts: green (<50%), yellow (50-75%), orange (75-90%), red (>90%)

### 2.3 Models Tab

```
+----------------------------------------------------------------------+-----------------------+
|  Model Comparison                                                    |  Today's Summary      |
|                                                                      |                       |
|  +------------------------------------------------------------------+|                       |
|  | Model   | Total Tokens | Avg/Session | Total Cost | Avg Latency ||  Model: Sonnet        |
|  |---------|--------------|-------------|------------|-------------||  Sessions: 12         |
|  | Opus    |    1,245,000 |      28,400 |    $67.20  |      4.2s   ||  Tokens: 145.2K       |
|  | Sonnet  |    2,890,000 |      12,100 |    $41.50  |      1.8s   ||  Cost: $3.47          |
|  | Haiku   |    4,120,000 |       8,200 |    $20.30  |      0.6s   ||                       |
|  +------------------------------------------------------------------+|  Budget (Daily)       |
|                                                                      |  ████████░░░░ 67%     |
|  Usage Distribution                                                  |  $3.47 / $5.00        |
|  Opus   : ████████████████████████████████████░░░░░░░░░░ 52%        |                       |
|  Sonnet : ████████████████████████░░░░░░░░░░░░░░░░░░░░░░ 32%        |  Budget (Monthly)     |
|  Haiku  : ████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ 16%        |  ██░░░░░░░░░░ 15%     |
|                                                                      |  $22.30 / $150.00     |
|  Token Breakdown per Model                                           |                       |
|  +------------------------------------------------------------------+|  Top Model Today      |
|  | Model   |   Input   |   Output   | Cache Read | Cache Write     ||  Sonnet  84%          |
|  |---------|-----------|------------|------------|-----------------|+  Opus    12%          |
|  | Opus    |   820,000 |    310,000 |    95,000  |      20,000     |  Haiku    4%          |
|  | Sonnet  | 1,890,000 |    640,000 |   290,000  |      70,000     |                       |
|  | Haiku   | 2,800,000 |    980,000 |   280,000  |      60,000     |                       |
|  +------------------------------------------------------------------+                       |
+----------------------------------------------------------------------+-----------------------+
```

**Table behavior:**
- Sortable columns: press `s` to cycle sort column, `S` to reverse
- Current sort column indicated with arrow: `Total Cost v` or `Total Cost ^`
- Selected row highlighted with primary color background
- Numbers right-aligned, names left-aligned
- Usage distribution uses `Gauge` widgets per model

### 2.4 Live Tab

#### Active Sessions State

```
+----------------------------------------------------------------------+-----------------------+
|  Live Sessions                                         2 active      |  Today's Summary      |
|                                                                      |                       |
|  +------------------------------------------------------------------+|  Model: Sonnet        |
|  | # | Session            | Model  | Status    | Tokens  | Duration||  Sessions: 12         |
|  |---|--------------------+--------+-----------+---------+---------||  Tokens: 145.2K       |
|  | 1 | claude-tui#a3f2    | Sonnet | Streaming | 12,450  | 2m 34s  ||  Cost: $3.47          |
|  | 2 | ml-pipeline#8b1c   | Opus   | Streaming |  8,200  | 1m 12s  ||                       |
|  | 3 | code-review#e4d9   | Haiku  | Idle      |  3,100  | 5m 48s  ||  Budget (Daily)       |
|  +------------------------------------------------------------------+|  ████████░░░░ 67%     |
|                                                                      |  $3.47 / $5.00        |
|  Session Detail: claude-tui#a3f2                                     |                       |
|  +------------------------------------------------------------------+|  Budget (Monthly)     |
|  | Model:    Sonnet                                                 ||  ██░░░░░░░░░░ 15%     |
|  | Status:   Streaming ...                                          ||  $22.30 / $150.00     |
|  | Started:  14:30:01 UTC                                           ||                       |
|  | Duration: 2m 34s                                                 ||  Top Model Today      |
|  | Input:    8,200 tokens                                           ||  Sonnet  84%          |
|  | Output:   4,250 tokens (streaming...)                            ||  Opus    12%          |
|  | Est Cost: $0.032                                                 ||  Haiku    4%          |
|  +------------------------------------------------------------------+|                       |
+----------------------------------------------------------------------+-----------------------+
```

**Streaming indicator animation:**
Braille spinner characters cycle at 100ms intervals:
`... -> ..` (3-dot pulse, simpler alternative to braille)

Full braille sequence for status column: ` ...`

#### Empty State

```
+----------------------------------------------------------------------+-----------------------+
|  Live Sessions                                         0 active      |  Today's Summary      |
|                                                                      |                       |
|                                                                      |  Model: --            |
|                                                                      |  Sessions: 0          |
|                                                                      |  Tokens: 0            |
|                                                                      |  Cost: $0.00          |
|                                                                      |                       |
|                     No active sessions                               |  Budget (Daily)       |
|                                                                      |  ░░░░░░░░░░░░  0%     |
|                Waiting for Claude activity...                        |  $0.00 / $5.00        |
|                                                                      |                       |
|                                                                      |  Budget (Monthly)     |
|                                                                      |  ░░░░░░░░░░░░  0%     |
|                                                                      |  $0.00 / $150.00      |
|                                                                      |                       |
|                                                                      |                       |
+----------------------------------------------------------------------+-----------------------+
```

- "No active sessions" centered both vertically and horizontally in content area
- Subtitle "Waiting for Claude activity..." in dim text below
- Sidebar shows zeroed-out summary

---

## 3. Color Scheme

### Core Palette

All colors defined as ratatui `Color` values. The scheme is designed for dark terminals but degrades gracefully on light terminals by using indexed colors where possible.

```rust
// -- Primary UI Colors --
const COLOR_PRIMARY:    Color = Color::Rgb(130, 170, 255);  // Soft blue - titles, active tabs
const COLOR_SECONDARY:  Color = Color::Rgb(140, 140, 160);  // Muted gray-blue - labels, borders
const COLOR_TEXT:        Color = Color::Reset;                // Terminal default foreground
const COLOR_TEXT_DIM:    Color = Color::Rgb(100, 100, 120);  // Dimmed text
const COLOR_BACKGROUND: Color = Color::Reset;                // Respect terminal default

// -- Status Colors --
const COLOR_SUCCESS:  Color = Color::Rgb(80, 200, 120);   // Green - connected, under budget
const COLOR_WARNING:  Color = Color::Rgb(240, 180, 50);   // Amber - approaching budget
const COLOR_ERROR:    Color = Color::Rgb(240, 80, 80);    // Red - disconnected, over budget
const COLOR_INFO:     Color = Color::Rgb(100, 180, 240);  // Light blue - informational

// -- Model Colors --
const COLOR_OPUS:   Color = Color::Rgb(200, 120, 255);  // Purple
const COLOR_SONNET: Color = Color::Rgb(80, 180, 240);   // Blue
const COLOR_HAIKU:  Color = Color::Rgb(120, 220, 180);  // Teal/mint

// -- Token Type Colors (for charts) --
const COLOR_INPUT_TOKENS:       Color = Color::Rgb(80, 180, 240);   // Blue
const COLOR_OUTPUT_TOKENS:      Color = Color::Rgb(240, 160, 80);   // Orange
const COLOR_CACHE_READ_TOKENS:  Color = Color::Rgb(140, 200, 100);  // Light green
const COLOR_CACHE_WRITE_TOKENS: Color = Color::Rgb(200, 140, 200);  // Light purple

// -- Budget Bar Gradient --
const COLOR_BUDGET_LOW:    Color = Color::Rgb(80, 200, 120);   // Green  (0-50%)
const COLOR_BUDGET_MED:    Color = Color::Rgb(240, 180, 50);   // Yellow (50-75%)
const COLOR_BUDGET_HIGH:   Color = Color::Rgb(240, 130, 50);   // Orange (75-90%)
const COLOR_BUDGET_CRIT:   Color = Color::Rgb(240, 80, 80);    // Red    (90-100%+)

// -- Active Tab Style --
const STYLE_TAB_ACTIVE: Style = Style::new()
    .fg(Color::Rgb(255, 255, 255))
    .bg(Color::Rgb(80, 100, 160))
    .add_modifier(Modifier::BOLD);

const STYLE_TAB_INACTIVE: Style = Style::new()
    .fg(Color::Rgb(140, 140, 160));
```

### Light Theme Fallback

For terminals with light backgrounds, the palette degrades using `Color::Indexed()` (256-color) values:

```rust
// When COLORFGBG or similar env hints indicate light theme:
const COLOR_PRIMARY_LIGHT:   Color = Color::Indexed(26);   // Dark blue
const COLOR_SECONDARY_LIGHT: Color = Color::Indexed(244);  // Medium gray
const COLOR_TEXT_DIM_LIGHT:  Color = Color::Indexed(249);  // Light gray
```

The app should detect light vs dark by checking the `COLORFGBG` environment variable or allowing a `--theme light|dark` CLI flag.

---

## 4. SketchyBar Plugin Design

### Item Format

```
[icon] [model] | [budget_bar] [pct]% | [$cost]
```

### Examples by State

**Active, under budget:**
```
 Sonnet | ████░░ 67% | $3.47
```

**Active, approaching budget (>75%):**
```
 Opus | █████░ 85% | $4.25
```

**Active, over budget:**
```
 Sonnet | ██████ 102% | $5.10
```

**Idle (no active sessions):**
```
 Idle | ████░░ 67% | $3.47
```

### Icon Choices

| State                | Icon | Unicode  |
|----------------------|------|----------|
| Active - streaming   |   | U+F0E7 (Nerd Font lightning) or plain: `*` |
| Idle - no sessions   |   | U+F111 (Nerd Font circle) or plain: `-` |
| Over budget          |   | U+F071 (Nerd Font warning) or plain: `!` |
| Disconnected         |   | U+F127 (Nerd Font unlink) or plain: `x` |

### Budget Bar Characters

Uses Unicode block elements for 6-character bar:
- `█` (U+2588) - filled
- `░` (U+2591) - empty

### Color Coding (SketchyBar label colors)

```bash
# Budget percentage thresholds
if [ "$budget_pct" -lt 50 ]; then
    COLOR="0xff50c878"   # Green
elif [ "$budget_pct" -lt 75 ]; then
    COLOR="0xfff0b432"   # Yellow
elif [ "$budget_pct" -lt 90 ]; then
    COLOR="0xfff08232"   # Orange
else
    COLOR="0xfff05050"   # Red
fi
```

### Update Interval

- **Active sessions:** Every 5 seconds
- **Idle:** Every 60 seconds
- **Triggered:** Immediately on session start/end events via daemon notification

---

## 5. Menu Bar Widget Design (menubar.sh)

### Compact Format

The macOS menu bar has limited space. The widget adapts based on available room.

**Full format (when space allows):**
```
 S | ████░░ 67% | $3.47
```

**Compact format:**
```
 S 67% $3.47
```

**Minimal format:**
```
S $3.47
```

### Model Abbreviations

| Model  | Abbreviation |
|--------|-------------|
| Opus   | O           |
| Sonnet | S           |
| Haiku  | H           |
| Mixed  | M           |
| None   | -           |

### Active/Idle Indicators

| State     | Symbol | Description         |
|-----------|--------|---------------------|
| Active    | ``    | Filled circle (U+25CF) |
| Idle      | ``    | Empty circle (U+25CB) |
| Error     | ``    | Warning sign (U+26A0) |

### Budget Bar (6 chars)

```
 0%:  ░░░░░░
17%:  █░░░░░
33%:  ██░░░░
50%:  ███░░░
67%:  ████░░
83%:  █████░
100%: ██████
```

### Truncation Rules

1. First drop: budget bar -> show only percentage
2. Second drop: percentage -> show only cost
3. Third drop: model abbreviation -> show only icon + cost
4. Never truncate: cost value (most important info)

---

## 6. AeroSpace Workspace Indicator

### Format

Displayed in AeroSpace workspace name or label overlay:

```
[model_abbrev][status_dot]
```

### Examples

```
S*    # Sonnet, active session
O*    # Opus, active session
H     # Haiku, idle
S     # Sonnet, idle
-     # No recent activity
```

### Status Dot

| State          | Symbol |
|----------------|--------|
| Active/Streaming | `*`  |
| Idle           | (none) |
| Over budget    | `!`    |

### Integration Method

The script writes the indicator to a temp file or outputs to stdout. AeroSpace reads it via a custom command or shell exec.

```bash
#!/bin/bash
# Query daemon for active model
RESULT=$(echo '{"method":"get_status"}' | socat - UNIX-CONNECT:/tmp/claude-daemon.sock)
MODEL=$(echo "$RESULT" | jq -r '.model // "-"')
ACTIVE=$(echo "$RESULT" | jq -r '.active // false')

case "$MODEL" in
    "Opus")   ABBREV="O" ;;
    "Sonnet") ABBREV="S" ;;
    "Haiku")  ABBREV="H" ;;
    *)        ABBREV="-" ;;
esac

if [ "$ACTIVE" = "true" ]; then
    echo "${ABBREV}*"
else
    echo "${ABBREV}"
fi
```

---

## 7. Widget Component Hierarchy

### Top-Level Layout Tree

```
Frame
  Layout::vertical [Header(1), TabBar(1), Content(Min(20)), Footer(1)]
    |
    +-- Header: Paragraph (styled spans for title + status + clock)
    |
    +-- TabBar: Tabs widget
    |
    +-- Content: Layout::horizontal [Main(Percentage(70)), Sidebar(Percentage(30))]
    |     |
    |     +-- Main: Block (bordered) -> [tab-specific content]
    |     |
    |     +-- Sidebar: Block (bordered, title "Today's Summary")
    |           |
    |           +-- Layout::vertical [
    |                 ModelInfo: Paragraph,
    |                 Sessions: Paragraph,
    |                 Tokens: Paragraph,
    |                 Cost: Paragraph,
    |                 Spacer: Length(1),
    |                 DailyBudgetLabel: Paragraph,
    |                 DailyBudget: Gauge,
    |                 DailyBudgetDetail: Paragraph,
    |                 Spacer: Length(1),
    |                 MonthlyBudgetLabel: Paragraph,
    |                 MonthlyBudget: Gauge,
    |                 MonthlyBudgetDetail: Paragraph,
    |                 Spacer: Length(1),
    |                 TopModelLabel: Paragraph,
    |                 TopModelBars: BarChart (horizontal)
    |               ]
    |
    +-- Footer: Paragraph (styled key hints)
```

### Tab-Specific Widget Trees

#### Tokens Tab

```
Layout::vertical [RangeSelector(1), Chart(Min(10)), Legend(1)]
  |
  +-- RangeSelector: Tabs widget (items: "7d", "30d", "90d")
  +-- Chart: BarChart
  |     bar_width: 3
  |     bar_gap: 1
  |     group_gap: 2
  |     BarGroup per date, each with 3 bars (input, output, cache)
  +-- Legend: Paragraph (colored spans for each token type)
```

#### Costs Tab

```
Layout::vertical [TopRow(Percentage(40)), ProjectTable(Min(6)), BudgetBar(3)]
  |
  +-- TopRow: Layout::horizontal [ModelBreakdown(50%), CumulativeChart(50%)]
  |     |
  |     +-- ModelBreakdown: Block -> BarChart (horizontal bars per model)
  |     +-- CumulativeChart: Block -> Sparkline (cumulative spend over time)
  |
  +-- ProjectTable: Block -> Table
  |     header: Row ["Project", "Model", "Cost", "Pct"]
  |     widths: [Percentage(35), Percentage(20), Percentage(25), Percentage(20)]
  |     highlight_style: primary color bg
  |     scrollable: true (state tracked in app)
  |
  +-- BudgetBar: Block -> Gauge
  |     ratio: budget_used / budget_total
  |     label: "$X.XX / $Y.YY"
  |     gauge_style: dynamic color based on percentage thresholds
```

#### Models Tab

```
Layout::vertical [ComparisonTable(Percentage(40)), UsageDist(Percentage(20)), BreakdownTable(Percentage(40))]
  |
  +-- ComparisonTable: Block -> Table
  |     header: Row ["Model", "Total Tokens", "Avg/Session", "Total Cost", "Avg Latency"]
  |     widths: [Percentage(15), Percentage(20), Percentage(20), Percentage(20), Percentage(25)]
  |     rows styled with respective model colors
  |
  +-- UsageDist: Block ("Usage Distribution")
  |     Layout::vertical [one Gauge per model]
  |     Each Gauge:
  |       label: "Model: XX%"
  |       gauge_style: model color
  |
  +-- BreakdownTable: Block -> Table
  |     header: Row ["Model", "Input", "Output", "Cache Read", "Cache Write"]
  |     widths: [Percentage(15), Percentage(20), Percentage(20), Percentage(25), Percentage(20)]
```

#### Live Tab

```
Layout::vertical [SessionList(Percentage(45)), SessionDetail(Percentage(55))]
  |
  +-- SessionList: Block (title with session count) -> Table
  |     header: Row ["#", "Session", "Model", "Status", "Tokens", "Duration"]
  |     widths: [Length(3), Percentage(30), Percentage(12), Percentage(18), Percentage(15), Percentage(15)]
  |     Status cell uses styled Span:
  |       "Streaming" + spinner char -> COLOR_SUCCESS
  |       "Idle" -> COLOR_WARNING
  |       "Completed" -> COLOR_TEXT_DIM
  |
  +-- SessionDetail: Block (title "Session Detail: {id}") -> Paragraph
  |     Each line is a key-value pair using styled spans
  |     Key in bold secondary color, value in default text
  |
  +-- (Empty state): Block -> Paragraph
  |     Centered text "No active sessions"
  |     Subtitle "Waiting for Claude activity..." in dim
```

### Sidebar Widget Detail

```
Block (title "Today's Summary", border: Borders::ALL, border_style: COLOR_SECONDARY)
  |
  +-- Paragraph: "Model: {name}" where name is colored by model color
  +-- Paragraph: "Sessions: {n}"
  +-- Paragraph: "Tokens: {n}" with K/M suffix formatting
  +-- Paragraph: "Cost: ${x.xx}"
  +-- Paragraph: "" (spacer)
  +-- Paragraph: "Budget (Daily)" in bold
  +-- Gauge:
  |     ratio: daily_spent / daily_limit
  |     label: "{pct}%"
  |     gauge_style: budget color gradient
  +-- Paragraph: "${spent} / ${limit}" in dim
  +-- Paragraph: "" (spacer)
  +-- Paragraph: "Budget (Monthly)" in bold
  +-- Gauge: (same pattern as daily)
  +-- Paragraph: "${spent} / ${limit}" in dim
  +-- Paragraph: "" (spacer)
  +-- Paragraph: "Top Model Today" in bold
  +-- Per model: Paragraph with inline gauge using block chars
```

---

## 8. Responsive Behavior

### Terminal Width Adaptation

| Width Range  | Behavior                                      |
|-------------|----------------------------------------------|
| >= 120 cols  | Full layout as designed                       |
| 100-119 cols | Sidebar narrows to 25%, abbreviated labels    |
| 80-99 cols   | Sidebar hidden, full-width content            |
| < 80 cols    | Warning: "Terminal too narrow" overlay         |

### Terminal Height Adaptation

| Height Range | Behavior                                      |
|-------------|----------------------------------------------|
| >= 40 rows   | Full layout with all sections                 |
| 30-39 rows   | Chart Y-axis labels reduced                   |
| < 30 rows    | Warning: "Terminal too short" overlay          |

### Resize Handling

The app should handle `SIGWINCH` (terminal resize signal) via crossterm's event system and re-render immediately.

---

## 9. Animation and Refresh Rates

| Element                | Refresh Rate  |
|------------------------|---------------|
| Clock in header        | Every 1 second|
| Live session table     | Every 2 seconds|
| Streaming spinner      | Every 100ms   |
| Token/Cost charts      | On data change or manual refresh (r)|
| Budget gauges          | Every 30 seconds|
| Connection status      | On state change|

### Spinner Sequence

For the streaming status indicator, use a simple dot animation:

```
Frame 0:  .
Frame 1:  ..
Frame 2:  ...
Frame 3:  ..
Frame 4:  .
Frame 5:  (empty)
```

Or the braille spinner from the design doc:

```
const SPINNER: &[char] = &['...', '.. ', '.  ', '   ', '  .', ' ..', '...'];
```

---

## 10. Number Formatting Rules

| Type        | Format               | Example         |
|-------------|----------------------|-----------------|
| Token count | Comma-separated      | 1,245,000       |
| Token short | K/M suffix           | 145.2K, 1.2M    |
| Cost        | 2 decimal places     | $3.47           |
| Percentage  | Integer (no decimal) | 67%             |
| Duration    | Xm Ys                | 2m 34s          |
| Date        | MMM DD               | Mar 05          |
| Time        | HH:MM:SS             | 14:32:01        |
| Latency     | 1 decimal + s        | 4.2s            |
