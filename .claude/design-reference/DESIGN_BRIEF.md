# Design Reference — Cockpit Energy

## Mood Board (4 screenshots in this directory)

### ref_01_palantir_gotham.png
- Dark theme, teal/cyan accent color
- Multi-panel: situation panel left, map center, course of action right, timeline bottom
- Four views of the same data, all live, all interactive
- Click a node on the map → left panel updates → timeline highlights
- Military-grade information density but still readable
- TAKEAWAY: multi-panel layout, dark theme, data-driven panels that cross-reference

### ref_02_financial_dashboard.png
- Maximum information density on one screen
- Charts, gauges, metrics, projections — no scrolling
- Color-coded status indicators (green/red/amber)
- Multiple chart types in one view (bar, line, gauge, pie)
- TAKEAWAY: everything at a glance, no wasted pixels, color = meaning

### ref_03_risk_map.png
- The scatter plot IS the interface, not output of a cell
- Select points → context menu → filter/add/clear
- Sidebar with breakdowns (histograms, bars) that update on selection
- Table below with sortable columns
- Timeline trend in corner
- TAKEAWAY: visualization is primary, data table is secondary, selection drives everything

### ref_04_metabase.png
- Parameters panel on left drives everything on right
- Multiple linked visualizations: histogram, map, table
- Change a filter → all panels reflow
- Clean, professional, not "developer tool" aesthetic
- TAKEAWAY: parameters drive views, multiple viz types coexist, non-technical users feel comfortable

## Design Principles Extracted

### Color
- Dark background (#0a0e17 to #1a1f2e range)
- Teal/cyan primary accent (#00bcd4 to #4dd0e1)
- Amber for warnings, red for errors, green for healthy
- White/light gray text on dark, not the reverse
- Subtle grid lines, not harsh borders

### Typography
- Clean sans-serif (Inter, SF Pro, or similar)
- Monospace only inside code cells, not for results
- Large numbers for KPIs (single scalar results)
- Small, dense text for tables — maximize rows visible

### Layout
- CSS Grid, not flexbox column stack
- Panels, not cells-in-a-list
- Sidebar always visible (properties, filters, or parameters)
- Graph visualization gets the most space
- Tables are compact and sortable, not scrollable card lists

### Interaction
- Click node → everything updates (selection is global state)
- Hover → tooltip with key properties
- Drag → rearrange graph layout
- Right-click → context menu (filter, expand, hide, inspect)
- No modal dialogs for configuration — inline controls

### Animation
- Graph layout settles like leaves on water (force simulation)
- Panel transitions: 200ms ease, not instant
- New results fade in, don't pop
- Loading: subtle pulse on the cell border, not a spinner

### Feel
- Premium. Not "developer tool." Not "academic notebook."
- A manager sees this in a demo and thinks "enterprise."
- A data engineer sees this and thinks "finally, a real tool."
- A Bardioc analyst sees this and thinks "I can use this."
