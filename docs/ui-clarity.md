# UI clarity and consistency

The existing address-book design remains the baseline: paper #f3f4f6, surface
#ffffff, ink #1b2230, secondary ink #545d6b, gamboge #f5b83d and danger #c43d2b.
All new layout uses the existing semantic variables, body/display fonts, spacing,
controls and source colours, including their existing dark-theme values.

The initial layout review identified unrelated actions spread across Ask Monica,
native file-input chrome, unexplained operational counters and settings without
navigation. The revised layout groups the conversation toolbar, person context,
transcript and composer in one bounded workspace. Empty-state prompts only fill
the question; they never send it automatically. Conversation import/export stays
available. Broken avatars fall back to initials without changing their dimensions.

Settings have addressable general/account/AI/background categories. Panels remain
mounted while changing categories, preserving unsaved form values. Models are
grouped by connection, task role and permitted content. Advanced token controls
remain available with units and explanations. Completed scheduling is labelled
as arranged work, while model execution pauses remain explicit. Technical task
IDs and historical totals remain available inside diagnostics.

Imports describe each stage, count unit and consequence. Global backlog counts
remain global while source/date filters apply to the records below; counts from
different stages cannot be added together. Media references are distinguished
from unique files. Missing telemetry is not presented as zero. Weekly windows
show their reliable captured denominator only after capture; reading all input
still leaves final writing. Reconciliation, repair and credentials stay available
in clearly named expandable sections. Contact association uses a compact opt-in
form with separate email/phone labels, validation and saved/error feedback.

Focused validation uses synthetic API responses only: Ask, imports, AI settings
and background status at 1440px/390px in both themes, with no document overflow
or browser script errors; prompt insertion and file chooser access; contact
submission and broken-avatar fallback; TypeScript and production web build.
No live NAS mutation, model request or collector activation is part of UI QA.
Fresh independent bugbot/code-review/excessive-tests review is required before
merge. Screenshots and request evidence are retained in the local validation
workspace rather than shipping synthetic personal data in the application.
