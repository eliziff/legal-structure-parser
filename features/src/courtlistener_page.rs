//! A case's page on CourtListener as its own code publishes it, printed from bulk data alone: the words Chrome's
//! text-fragment finder searches there, one line per block, with the page's ids and the marks it prints beside the words.
//!
//! Each rule names the file of freelawproject/courtlistener it reproduces (`opinion_tabs_content.html`,
//! `opinions.html`, `opinions.js`, `opinions.css`, `override.css`, models.py's `ordered_opinions`). The opinion's markup
//! is parsed as a browser parses it (html5ever), the page's script is applied to the tree, and the tree is read as the
//! finder reads it (LESSONS.md "Browser facts"): a block boundary at `<br>` and around everything the stylesheets lay out
//! other than inline, hidden text dropped, generated content (`::before`, `::after`) never read.

use scraper::{Html, Node as HtmlNode};

/// One opinion of a case as CourtListener's bulk data holds it (opinions-2026-09-30.csv): each rendition, empty where
/// the record has none.
#[derive(Clone, Debug, Default)]
pub struct CourtListenerOpinion {
    pub id: u64,
    /// Its `type`, `010combined`, `020lead`, `040dissent`...
    pub kind: String,
    pub author_str: String,
    /// Whether `author_id` names a judge: the page then prints the judge's full name, which bulk opinions do not hold.
    pub has_author: bool,
    pub extracted_by_ocr: bool,
    pub html_with_citations: String,
    pub xml_harvard: String,
    pub xml_scan: String,
    pub html: String,
    pub html_columbia: String,
    pub html_lawbox: String,
    pub html_anon_2020: String,
    pub plain_text: String,
}

/// A cluster's fields as bulk data holds them (opinion-clusters-2026-09-30.csv), empty where absent, and its opinions
/// in the order the page prints them.
#[derive(Clone, Debug, Default)]
pub struct CourtListenerCluster {
    pub source: String,
    pub case_name: String,
    pub case_name_full: String,
    pub case_name_short: String,
    pub date_filed: String,
    pub judges: String,
    pub posture: String,
    pub nature_of_suit: String,
    pub other_dates: String,
    pub precedential_status: String,
    pub headmatter: String,
    pub correction: String,
    pub attorneys: String,
    pub headnotes: String,
    pub syllabus: String,
    pub summary: String,
    pub history: String,
    pub disposition: String,
    pub opinions: Vec<CourtListenerOpinion>,
}

/// A place a CourtListener page can be linked at: `id` an element's id, `kind` `page` (a star page), `block` (a
/// Harvard paragraph, a resource.org numbered paragraph), `section` (the `o1` of a case's nth opinion, `o` its head
/// matter, `attorneys`...), `note` (a footnote) or `call` (a note's call); `at` a byte of the page's text. `unique`: the
/// page prints that id once.
#[derive(Clone, Debug, serde::Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct CourtListenerAnchor {
    pub id: String,
    pub kind: &'static str,
    pub at: usize,
    pub unique: bool,
}

/// The page's text, its marks (page labels and note calls, printed beside the words) and its anchors; `article` the
/// byte where `#opinion > article` begins.
pub(crate) struct PrintedPage {
    pub text: String,
    pub marks: Vec<(usize, usize)>,
    pub anchors: Vec<CourtListenerAnchor>,
    pub article: usize,
}

// ---------------------------------------------------------------------------------------------------------------
// The tree: html5ever's, copied into one that the page's script can rewrite.

#[derive(Clone, Debug)]
enum Item {
    Element { name: String, attrs: Vec<(String, String)> },
    Text(String),
}

struct Dom {
    items: Vec<Item>,
    children: Vec<Vec<usize>>,
    parent: Vec<Option<usize>>,
}

impl Dom {
    /// `markup` parsed as a browser parses it inside the page's body.
    fn parse(markup: &str) -> Self {
        let html = Html::parse_fragment(markup);
        let mut dom = Dom { items: Vec::new(), children: Vec::new(), parent: Vec::new() };
        let root = dom.push(None, Item::Element { name: "#root".into(), attrs: Vec::new() });
        fn copy(dom: &mut Dom, parent: usize, node: ego_tree::NodeRef<'_, HtmlNode>) {
            for child in node.children() {
                match child.value() {
                    HtmlNode::Element(element) => {
                        let attrs = element.attrs().map(|(name, value)| (name.to_owned(), value.to_owned())).collect();
                        let at = dom.push(Some(parent), Item::Element { name: element.name().to_owned(), attrs });
                        copy(dom, at, child);
                    }
                    HtmlNode::Text(text) => {
                        dom.push(Some(parent), Item::Text(text.text.to_string()));
                    }
                    _ => {}
                }
            }
        }
        // A fragment's tree holds its nodes under one <html> element.
        copy(&mut dom, root, *html.root_element());
        dom
    }

    fn push(&mut self, parent: Option<usize>, item: Item) -> usize {
        let at = self.items.len();
        self.items.push(item);
        self.children.push(Vec::new());
        self.parent.push(parent);
        if let Some(parent) = parent {
            self.children[parent].push(at);
        }
        at
    }

    fn name(&self, at: usize) -> &str {
        match &self.items[at] {
            Item::Element { name, .. } => name,
            Item::Text(_) => "",
        }
    }

    fn attr(&self, at: usize, wanted: &str) -> Option<&str> {
        match &self.items[at] {
            Item::Element { attrs, .. } => attrs.iter().find(|(name, _)| name == wanted).map(|(_, value)| value.as_str()),
            Item::Text(_) => None,
        }
    }

    fn set_attr(&mut self, at: usize, wanted: &str, value: &str) {
        if let Item::Element { attrs, .. } = &mut self.items[at] {
            match attrs.iter_mut().find(|(name, _)| name == wanted) {
                Some(slot) => slot.1 = value.to_owned(),
                None => attrs.push((wanted.to_owned(), value.to_owned())),
            }
        }
    }

    fn has_class(&self, at: usize, class: &str) -> bool {
        self.attr(at, "class").is_some_and(|classes| classes.split_ascii_whitespace().any(|word| word == class))
    }

    fn is(&self, at: usize, name: &str) -> bool {
        matches!(&self.items[at], Item::Element { name: own, .. } if own == name)
    }

    fn element_parent(&self, at: usize) -> Option<usize> {
        self.parent[at].filter(|parent| self.name(*parent) != "#root")
    }

    fn ancestors(&self, at: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.element_parent(at), move |at| self.element_parent(*at))
    }

    /// Every element in document order, as a selector query returns them.
    fn elements(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![0];
        while let Some(at) = stack.pop() {
            if at != 0 && matches!(self.items[at], Item::Element { .. }) {
                out.push(at);
            }
            stack.extend(self.children[at].iter().rev());
        }
        out
    }

    fn text_content(&self, at: usize) -> String {
        match &self.items[at] {
            Item::Text(text) => text.clone(),
            Item::Element { .. } => self.children[at].iter().map(|child| self.text_content(*child)).collect(),
        }
    }

    /// jQuery's `replaceWith` of a new element taking the old one's content: the same node renamed, its attributes
    /// replaced.
    fn rename(&mut self, at: usize, name: &str, attrs: Vec<(String, String)>) {
        self.items[at] = Item::Element { name: name.to_owned(), attrs };
    }

    fn set_text(&mut self, at: usize, text: &str) {
        for child in std::mem::take(&mut self.children[at]) {
            self.parent[child] = None;
        }
        self.push(Some(at), Item::Text(text.to_owned()));
    }

    fn detach(&mut self, at: usize) {
        if let Some(parent) = self.parent[at].take() {
            self.children[parent].retain(|child| *child != at);
        }
    }

    fn attached(&self, at: usize) -> bool {
        std::iter::successors(Some(at), |at| self.parent[*at]).last() == Some(0)
    }

    fn append(&mut self, parent: usize, name: &str, attrs: Vec<(String, String)>, text: &str) {
        let at = self.push(Some(parent), Item::Element { name: name.to_owned(), attrs });
        self.push(Some(at), Item::Text(text.to_owned()));
    }
}

fn attrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The page's script (cl/assets/static-global/js/opinions.js, as served since 2025-03-14: 59889cac7fcf, 0b327e00549b).

fn apply_page_script(dom: &mut Dom) {
    // Star pagination for ANON 2020 (lines 1-21): of a pagescheme's two numbers only the one without 'P' is shown.
    for at in dom.elements() {
        if !dom.has_class(at, "star-pagination") {
            continue;
        }
        let label = if dom.attr(at, "pagescheme").is_some() {
            let number = dom.attr(at, "number").unwrap_or("").to_owned();
            if number.contains('P') { String::new() } else { number }
        } else {
            dom.text_content(at).trim().replacen("*Page ", "", 1)
        };
        dom.set_attr(at, "label", &label);
    }
    // <page-number> and span.star-pagination become a.page-label "*{label}" (lines 23-64). A label missing is a
    // TypeError there: the script stops, and the page keeps the rest as served.
    for at in dom.elements() {
        if !(dom.is(at, "page-number") || (dom.is(at, "span") && dom.has_class(at, "star-pagination"))) {
            continue;
        }
        let Some(label) = dom.attr(at, "label").map(|label| label.replacen('*', "", 1).trim().to_owned()) else { return };
        let index = dom.attr(at, "citation-index").map(str::to_owned);
        let mut new = vec![("class".to_owned(), "page-label".to_owned())];
        if let Some(index) = index {
            new.push(("data-citation-index".to_owned(), index));
        }
        new.extend(attrs(&[("data-label", &label), ("href", &format!("#{label}")), ("id", &label)]));
        dom.rename(at, "a", new);
        dom.set_text(at, &format!("*{label}"));
    }
    // `.footnotes > .footnote` becomes <footnote>, its attributes kept (lines 89-99).
    for at in dom.elements() {
        if dom.has_class(at, "footnote") && dom.element_parent(at).is_some_and(|parent| dom.has_class(parent, "footnotes")) {
            if let Item::Element { attrs, .. } = dom.items[at].clone() {
                dom.rename(at, "footnote", attrs);
            }
        }
    }
    // `div.footnote > a` removed (line 102).
    for at in dom.elements() {
        if dom.is(at, "a") && dom.element_parent(at).is_some_and(|parent| dom.is(parent, "div") && dom.has_class(parent, "footnote")) {
            dom.detach(at);
        }
    }
    // a.footnote and div.footnote pair by index where their counts match (lines 103-129). The attribute copies there
    // read `.attributes` of a jQuery object, which is undefined: the new elements take none but a footnote's label,
    // and `attr('label', undefined)` sets nothing.
    let live = |dom: &Dom| dom.elements().into_iter().filter(|at| dom.attached(*at)).collect::<Vec<_>>();
    let elements = live(dom);
    let marks = elements.iter().copied().filter(|at| dom.is(*at, "a") && dom.has_class(*at, "footnote")).collect::<Vec<_>>();
    let notes = elements.iter().copied().filter(|at| dom.is(*at, "div") && dom.has_class(*at, "footnote")).collect::<Vec<_>>();
    if marks.len() == notes.len() {
        for (mark, note) in marks.into_iter().zip(notes) {
            dom.rename(mark, "footnotemark", Vec::new());
            let label = dom.attr(note, "label").map(str::to_owned);
            dom.rename(note, "footnote", label.map(|label| vec![("label".to_owned(), label)]).unwrap_or_default());
        }
    }
    // footnotemark and footnote (not orphan="true") pair by index where their counts match: the mark becomes
    // <sup><a id="fnref{i}" href="#fn{i}">, the note takes id fn{i} and a jumpback; else every note takes a jumpback
    // (lines 136-170).
    let elements = live(dom);
    let marks = elements.iter().copied().filter(|at| dom.is(*at, "footnotemark")).collect::<Vec<_>>();
    let notes = elements.iter().copied().filter(|at| dom.is(*at, "footnote") && dom.attr(*at, "orphan") != Some("true")).collect::<Vec<_>>();
    if marks.len() == notes.len() {
        for (index, (mark, note)) in marks.into_iter().zip(notes).enumerate() {
            dom.rename(mark, "sup", Vec::new());
            let inner = std::mem::take(&mut dom.children[mark]);
            let link = dom.push(Some(mark), Item::Element { name: "a".into(),
                attrs: attrs(&[("href", &format!("#fn{index}")), ("id", &format!("fnref{index}"))]) });
            for child in inner {
                dom.parent[child] = Some(link);
                dom.children[link].push(child);
            }
            dom.set_attr(note, "id", &format!("fn{index}"));
            dom.append(note, "a", attrs(&[("class", "jumpback"), ("href", &format!("#fnref{index}"))]), "↵");
        }
    } else {
        for note in notes {
            let label = dom.attr(note, "label").unwrap_or("").to_owned();
            dom.append(note, "a", attrs(&[("class", "jumpback"), ("label", &label)]), "↵");
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The stylesheets: how each element is laid out, as far as the finder is concerned.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Not rendered: its text is never searched.
    Hidden,
    Inline,
    /// Laid out other than inline: a block, table part, list item, inline-block, float or absolute position.
    Block,
}

/// The inline `style` attribute's word on `property`, where it gives one.
fn style_value<'a>(dom: &'a Dom, at: usize, property: &str) -> Option<&'a str> {
    dom.attr(at, "style")?.split(';').filter_map(|declaration| declaration.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case(property)).map(|(_, value)| value.trim()).last()
}

/// The user agent's display of an element (the HTML standard's rendering section, as Chrome draws it).
fn default_layout(name: &str) -> Layout {
    match name {
        "head" | "script" | "style" | "template" | "title" | "meta" | "link" | "base" | "noscript" | "datalist" | "area"
        | "param" | "rp" | "select" | "textarea" | "input" | "button" | "option" | "optgroup" => Layout::Hidden,
        "address" | "article" | "aside" | "blockquote" | "body" | "center" | "dd" | "details" | "dialog" | "dir" | "div"
        | "dl" | "dt" | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "frameset" | "h1" | "h2" | "h3" | "h4"
        | "h5" | "h6" | "header" | "hgroup" | "hr" | "legend" | "li" | "listing" | "main" | "menu" | "nav" | "ol" | "p"
        | "plaintext" | "pre" | "search" | "section" | "summary" | "table" | "caption" | "colgroup" | "col" | "thead"
        | "tbody" | "tfoot" | "tr" | "td" | "th" | "ul" | "xmp" | "html" | "iframe" | "marquee" => Layout::Block,
        _ => Layout::Inline,
    }
}

fn layout(dom: &Dom, at: usize) -> Layout {
    let name = dom.name(at);
    // Form controls are not searched (LESSONS.md "Browser facts": select, input values, buttons).
    if matches!(name, "select" | "textarea" | "input" | "button" | "script" | "style" | "template" | "noscript" | "head"
        | "title" | "svg" | "math") {
        return Layout::Hidden;
    }
    if dom.attr(at, "hidden").is_some() {
        return Layout::Hidden;
    }
    let parent = dom.element_parent(at);
    let parent_is = |name: &str| parent.is_some_and(|parent| dom.is(parent, name));
    let grandparent = parent.and_then(|parent| dom.element_parent(parent));
    let under_harvard_opinion = parent_is("opinion") && grandparent.is_some_and(|at| dom.has_class(at, "harvard"));
    let inside = |test: &dyn Fn(usize) -> bool| dom.ancestors(at).any(test);
    let inside_id = |id: &str| inside(&|at| dom.attr(at, "id") == Some(id));
    let class = |word: &str| dom.has_class(at, word);
    // The page's stylesheets in their order: opinions.css, bootstrap.min.css 3.3.0, override.css; at a 1280px wide
    // window. First what none overrides: bootstrap's !important and its hidden parts.
    if bootstrap_hidden(dom, at) {
        return Layout::Hidden;
    }
    // The inline style, above every stylesheet rule but those.
    if let Some(display) = style_value(dom, at, "display") {
        return match display.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str() {
            "none" => Layout::Hidden,
            "inline" | "contents" => floated(dom, at, Layout::Inline),
            _ => Layout::Block,
        };
    }
    let author = (|| {
        // opinions.css (wrapped in .opinion-body, which the page's <body> carries), most specific first.
        if class("jumpback") && inside_id("resource-org-text") {
            return Some(Layout::Hidden);
        }
        if inside_id("headmatter") {
            let headmatter_child = parent.is_some_and(|parent| dom.attr(parent, "id") == Some("headmatter"));
            let scanned_child = parent_is("headmatter") && grandparent.is_some_and(|at| dom.attr(at, "id") == Some("headmatter"));
            let inside_scanned = dom.ancestors(at).any(|up| dom.is(up, "headmatter")
                && dom.element_parent(up).is_some_and(|at| dom.attr(at, "id") == Some("headmatter")));
            if (headmatter_child && name == "parties")
                || matches!(name, "docketnumber" | "court" | "parties" | "attorneys" | "syllabus" | "decisiondate")
                || (scanned_child && matches!(name, "citation" | "p" | "history" | "judges"))
                || (inside_scanned && matches!(name, "party" | "separator")) {
                return Some(Layout::Block);
            }
        }
        if under_harvard_opinion && name == "heading" {
            return Some(Layout::Block);
        }
        if under_harvard_opinion && name == "author" {
            return Some(Layout::Block); // inline-block
        }
        if name == "img" && parent_is("figure") && grandparent.is_some_and(|at| dom.is(at, "opinion")) {
            return Some(Layout::Block);
        }
        if name == "sup" && dom.attr(at, "id") == Some("fn1") && inside(&|at| dom.is(at, "footnote_body")) {
            return Some(Layout::Block);
        }
        if name == "p" && inside(&|at| dom.has_class(at, "scraped-html")) {
            return Some(Layout::Block);
        }
        if name == "p" && parent.is_some_and(|parent| (dom.is(parent, "div") && dom.has_class(parent, "footnote")) || dom.is(parent, "footnote")) {
            return Some(Layout::Inline);
        }
        if (name == "div" && class("footnote")) || name == "footnote" || name == "blockquote" {
            return Some(Layout::Block);
        }
        if name == "page-number" && parent_is("author") {
            return Some(Layout::Block);
        }
        // override.css: a Harvard note's own call, set absolute.
        if name == "a" && parent.is_some_and(|parent| dom.has_class(parent, "footnote")
            && dom.element_parent(parent).is_some_and(|up| dom.has_class(up, "footnotes")
                && dom.element_parent(up).is_some_and(|up| dom.is(up, "opinion")
                    && dom.element_parent(up).is_some_and(|up| dom.has_class(up, "harvard"))))) {
            return Some(Layout::Block);
        }
        if name == "a" && class("footnote") && inside_id("resource-org-text")
            && dom.ancestors(at).any(|up| dom.is(up, "div") && dom.has_class(up, "footnote")) {
            return Some(Layout::Block);
        }
        // override.css's class shortcuts, then bootstrap's class rules: as specific as `[id^="A"]` and later, so above it.
        if class("inline") {
            return Some(Layout::Inline);
        }
        if bootstrap_classed(dom, at) {
            return Some(Layout::Block);
        }
        // `[id^="A"] { display: inline }`, outside .opinion-body: a Harvard paragraph runs on in the line it
        // continues, unless a rule above names it.
        if dom.attr(at, "id").is_some_and(|id| id.starts_with('A')) {
            return Some(Layout::Inline);
        }
        // bootstrap's rules on elements alone, less specific than that.
        let in_blockquote = inside(&|at| dom.is(at, "blockquote"));
        if matches!(name, "label" | "legend" | "output" | "audio" | "canvas" | "progress" | "video")
            || (in_blockquote && (matches!(name, "footer" | "small") || class("small"))) {
            return Some(Layout::Block);
        }
        None
    })();
    let display = author.unwrap_or_else(|| default_layout(name));
    if display == Layout::Hidden {
        return display;
    }
    floated(dom, at, display)
}

/// bootstrap.min.css 3.3.0 and override.css at a 1280px wide screen: what they hide.
fn bootstrap_hidden(dom: &Dom, at: usize) -> bool {
    let class = |word: &str| dom.has_class(at, word);
    let parent_has = |word: &str| dom.element_parent(at).is_some_and(|parent| dom.has_class(parent, word));
    let classes = dom.attr(at, "class").unwrap_or("");
    class("hidden") || class("hide") || class("hidden-lg") || class("modal") || class("popover") || class("navbar-toggle")
        || classes.split_ascii_whitespace().any(|word| ["visible-xs", "visible-sm", "visible-md", "visible-print"].iter()
            .any(|prefix| word == *prefix || word.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('-'))))
        || (class("collapse") && !class("in") && !class("navbar-collapse")) || (class("dropdown-menu") && !parent_has("open"))
        || (class("tab-pane") && !class("active") && parent_has("tab-content"))
        || (dom.is(at, "audio") && dom.attr(at, "controls").is_none())
        || dom.attr(at, "data-loading").is_some() || class("htmx-hidden-indicator")
}

/// bootstrap.min.css 3.3.0 and override.css: the class rules that lay an element out other than inline (blocks,
/// inline-blocks, table cells, floats, absolute positions) at a 1280px wide screen.
fn bootstrap_classed(dom: &Dom, at: usize) -> bool {
    const CLASSES: &[&str] = &["img-responsive", "img-thumbnail", "sr-only", "form-control", "radio", "checkbox",
        "radio-inline", "checkbox-inline", "form-control-feedback", "help-block", "btn", "btn-block", "caret", "btn-group",
        "btn-group-vertical", "btn-group-justified", "input-group", "input-group-addon", "input-group-btn", "navbar-header",
        "navbar-brand", "navbar-nav", "navbar-text", "navbar-left", "navbar-right", "pagination", "badge", "thumbnail",
        "progress-bar", "media-left", "media-right", "media-body", "list-group-item", "embed-responsive", "close", "tooltip",
        "center-block", "pull-right", "pull-left", "show", "affix", "visible-lg", "visible-lg-block", "visible-lg-inline-block",
        "inline-block", "flex", "tag", "centered-tabs", "left-tabs", "cl-form-group"];
    let classes = dom.attr(at, "class").unwrap_or("");
    let parent = dom.element_parent(at);
    let parent_has = |word: &str| parent.is_some_and(|parent| dom.has_class(parent, word));
    let name = dom.name(at);
    classes.split_ascii_whitespace().any(|word| CLASSES.contains(&word)
        || ["col-xs-", "col-sm-", "col-md-", "col-lg-"].iter().any(|prefix| word.strip_prefix(prefix).is_some_and(|rest| rest.parse::<u8>().is_ok())))
        || (name == "li" && (parent_has("list-inline") || parent_has("nav") || parent_has("breadcrumb") || parent_has("pager")))
        || (name == "dt" && parent_has("dl-horizontal"))
        // A flex container's children are laid out as blocks.
        || parent.is_some_and(|parent| dom.has_class(parent, "flex") || style_value(dom, parent, "display")
            .is_some_and(|value| value.contains("flex") || value.contains("grid")))
}

/// A float or an absolute position is laid out apart from the line: bootstrap's `.pull-left`, `.pull-right`, an
/// image's or table's `align`, an inline style's.
fn floated(dom: &Dom, at: usize, display: Layout) -> Layout {
    let floats = dom.has_class(at, "pull-left") || dom.has_class(at, "pull-right")
        || (matches!(dom.name(at), "img" | "table") && dom.attr(at, "align").is_some_and(|align| align.eq_ignore_ascii_case("left") || align.eq_ignore_ascii_case("right")))
        || style_value(dom, at, "float").is_some_and(|value| !value.eq_ignore_ascii_case("none"))
        || style_value(dom, at, "position").is_some_and(|value| value.eq_ignore_ascii_case("absolute") || value.eq_ignore_ascii_case("fixed"));
    if floats { Layout::Block } else { display }
}

fn preformatted(dom: &Dom, at: usize) -> bool {
    if let Some(value) = style_value(dom, at, "white-space") {
        return value.starts_with("pre") || value == "break-spaces";
    }
    // override.css: `.plaintext .citation { white-space: pre }`, `.nowrap { white-space: nowrap }`.
    if dom.has_class(at, "citation") && dom.ancestors(at).any(|up| dom.has_class(up, "plaintext")) {
        return true;
    }
    matches!(dom.name(at), "pre" | "listing" | "xmp" | "plaintext" | "textarea")
}

fn hidden_by_visibility(dom: &Dom, at: usize) -> Option<bool> {
    if dom.has_class(at, "invisible") {
        return Some(true);
    }
    style_value(dom, at, "visibility").map(|value| !value.eq_ignore_ascii_case("visible"))
}

// ---------------------------------------------------------------------------------------------------------------
// Reading the tree as the finder does.

#[derive(Default)]
struct Printer {
    text: String,
    marks: Vec<(usize, usize)>,
    anchors: Vec<CourtListenerAnchor>,
}

impl Printer {
    fn end(&mut self) {
        while self.text.ends_with(' ') {
            self.text.pop();
        }
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    fn at_line_start(&self) -> bool {
        self.text.is_empty() || self.text.ends_with('\n')
    }

    fn words(&mut self, value: &str, pre: bool) {
        if pre {
            for (index, piece) in value.replace('\r', "").split('\n').enumerate() {
                if index > 0 {
                    self.end();
                }
                self.text.push_str(piece);
            }
            return;
        }
        let mut space = false;
        for character in value.chars() {
            if matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                space = true;
                continue;
            }
            if space && !self.at_line_start() && !self.text.ends_with(' ') {
                self.text.push(' ');
            }
            space = false;
            self.text.push(character);
        }
        if space && !self.at_line_start() && !self.text.ends_with(' ') {
            self.text.push(' ');
        }
    }

    fn anchor(&mut self, id: &str, kind: &'static str) {
        self.anchors.push(CourtListenerAnchor { id: id.to_owned(), kind, at: self.text.len(), unique: false });
    }

    /// Everything under `at`, `pre` inherited, `visible` the inherited visibility.
    fn read(&mut self, dom: &Dom, at: usize, pre: bool, visible: bool) {
        match &dom.items[at] {
            Item::Text(text) => {
                if visible {
                    self.words(text, pre);
                }
            }
            Item::Element { name, .. } => {
                let name = name.as_str();
                if name == "br" {
                    self.end();
                    return;
                }
                let layout = if name == "#root" { Layout::Inline } else { layout(dom, at) };
                if layout == Layout::Hidden {
                    return;
                }
                let visible = hidden_by_visibility(dom, at).map_or(visible, |hidden| !hidden);
                let pre = pre || preformatted(dom, at);
                if layout == Layout::Block {
                    self.end();
                }
                // The template's own elements (`#opinion`, `#harvard-text`...) are no passage's anchors; its section
                // titles are.
                let template = dom.attr(at, "data-template").is_some();
                if let Some(id) = dom.attr(at, "id").filter(|id| !id.is_empty()) {
                    let kind = if dom.has_class(at, "opinion-section-title") { Some("section") }
                        else if template { None }
                        else if name == "a" && dom.has_class(at, "page-label") { Some("page") }
                        else if name == "footnote" || (name == "div" && dom.has_class(at, "footnote")) { Some("note") }
                        else if id.starts_with("fnref") { Some("call") }
                        else { Some("block") };
                    if let Some(kind) = kind {
                        self.anchor(id, kind);
                    }
                }
                // Page labels and note calls are printed beside the words.
                let mark = (name == "a" && (dom.has_class(at, "page-label") || dom.has_class(at, "footnote")))
                    || name == "sup" || name == "footnotemark";
                let start = self.text.len();
                for child in &dom.children[at] {
                    self.read(dom, *child, pre, visible);
                }
                if mark && self.text.len() > start {
                    self.marks.push((start, self.text.len()));
                }
                if layout == Layout::Block {
                    self.end();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The templates.

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// Django's `linebreaksbr`: each newline a `<br>`.
fn linebreaksbr(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "<br>")
}

/// models.py `Opinion.OPINION_TYPES`.
fn type_display(kind: &str) -> &str {
    match kind {
        "010combined" => "Combined Opinion",
        "015unamimous" => "Unanimous Opinion",
        "020lead" => "Lead Opinion",
        "025plurality" => "Plurality Opinion",
        "030concurrence" => "Concurrence Opinion",
        "035concurrenceinpart" => "In Part Opinion",
        "040dissent" => "Dissent",
        "050addendum" => "Addendum",
        "060remittitur" => "Remittitur",
        "070rehearing" => "Rehearing",
        "080onthemerits" => "On the Merits",
        "090onmotiontostrike" => "On Motion to Strike Cost Bill",
        "100trialcourt" => "Trial Court Document",
        other => other,
    }
}

/// The opinion tab (cl/opinion_page/templates/includes/opinion_tabs_content.html, `#opinion`, lines 345-473): the
/// head matter, else the cluster's correction, attorneys, headnotes, syllabus, summary, history and disposition; then
/// each opinion under its title, in the rendition the template picks. The template's whitespace between its tags is
/// kept, since it decides where a parser reopens a formatting element left open in the data (an unclosed `<b>` in a
/// summary wraps everything after it, but not every element alike). The template's own elements carry
/// `data-template` here, which no rule reads.
fn opinion_tab(cluster: &CourtListenerCluster) -> String {
    let mut html = String::from("<div data-template class=\"tab-pane fade in active\" id=\"opinion\">\n<article data-template>\n");
    let title = |html: &mut String, tag: &str, id: &str, title: &str| {
        html.push_str(&format!("\n<{tag} class=\"opinion-section-title jump-link\" id=\"{id}\">{title}</{tag}>\n<hr class=\"hr-opinion\">\n"));
    };
    if !cluster.headmatter.is_empty() {
        title(&mut html, "h2", "o", "Headmatter");
        html.push_str(&format!("<div data-template class=\"serif-text harvard\" id=\"headmatter\">\n{}\n</div>\n", cluster.headmatter));
    } else {
        let harvard = |html: &mut String, id: &str, name: &str, value: &str| {
            if !value.is_empty() {
                title(html, "h2", id, name);
                html.push_str(&format!("<div data-template id=\"\" class=\"serif-text harvard\">\n{value}\n</div>\n"));
            }
        };
        harvard(&mut html, "correction", "Correction", &cluster.correction);
        if !cluster.attorneys.is_empty() {
            title(&mut html, "h2", "attorneys", "Attorneys");
            html.push_str(&format!("<div data-template class=\"serif-text harvard\">\n<p>{}</p>\n</div>\n", linebreaksbr(&cluster.attorneys)));
        }
        if !cluster.headnotes.is_empty() {
            title(&mut html, "h2", "headnotes", "Headnotes");
            html.push_str(&format!("<p class=\"bottom\">{}</p>\n", cluster.headnotes));
        }
        harvard(&mut html, "syllabus", "Syllabus", &cluster.syllabus);
        harvard(&mut html, "summary", "Summary", &cluster.summary);
        harvard(&mut html, "history", "History", &cluster.history);
        harvard(&mut html, "disposition", "Disposition", &cluster.disposition);
    }
    let source = &cluster.source;
    let text_id = if source.contains('U') { "harvard-text" } else if source.contains('Z') { "columbia-text" }
        else if source.contains('L') { "lawbox-text" } else if source.contains('R') { "resource-org-text" } else { "default-text" };
    for (index, opinion) in printed_opinions(cluster).into_iter().enumerate() {
        // The author's name: the judge's full name where `author` is set, which bulk opinions do not hold; their
        // author_str stands in for it.
        let by = if opinion.author_str.is_empty() { String::new() } else { format!("\n by {}\n", escaped(&opinion.author_str)) };
        title(&mut html, "h3", &format!("o{}", index + 1), &format!("\n{}\n{by}", type_display(&opinion.kind)));
        // The template opens this div and never closes it: each opinion stands inside the one before.
        html.push_str(&format!("\n<div data-template id=\"{text_id}\" class=\"v-offset-above-2\">\n\n<div data-template class=\"subopinion-content\">\n"));
        let o = opinion;
        let (class, body) = if !o.xml_harvard.is_empty() && !o.html_with_citations.is_empty() {
            ("serif-text harvard", o.html_with_citations.clone())
        } else if !o.xml_harvard.is_empty() {
            ("serif-text harvard", o.xml_harvard.clone())
        } else if !o.xml_scan.is_empty() && !o.html_with_citations.is_empty() {
            ("serif-text harvard", o.html_with_citations.clone())
        } else if !o.xml_scan.is_empty() {
            ("serif-text harvard", o.xml_scan.clone())
        } else if !o.html_with_citations.is_empty() {
            if source == "C" {
                // A court's PDF: scraped HTML keeps its own lines; text extracted from the PDF has each newline a <br>.
                if !o.html.is_empty() { ("serif-text scraped-html", o.html_with_citations.clone()) }
                else { ("plaintext", linebreaksbr(&o.html_with_citations)) }
            } else {
                ("serif-text", o.html_with_citations.clone())
            }
        } else if !o.html_columbia.is_empty() {
            ("serif-text", o.html_columbia.clone())
        } else if !o.html_lawbox.is_empty() {
            ("serif-text", o.html_lawbox.clone())
        } else if !o.html_anon_2020.is_empty() {
            ("serif-text", o.html_anon_2020.clone())
        } else if !o.html.is_empty() {
            ("serif-text", o.html.clone())
        } else {
            ("", format!("<pre>{}</pre>", escaped(&o.plain_text)))
        };
        if class.is_empty() {
            html.push_str(&body);
        } else {
            html.push_str(&format!("<div data-template class=\"{class}\">{body}</div>"));
        }
        html.push_str("\n</div>\n");
    }
    html.push_str("\n</article>\n</div>\n");
    html
}

/// The opinions the page prints, in its order: models.py `OpinionCluster.ordered_opinions` orders them by
/// `ordering_key` and, of several, keeps those that have one; where any is a version of another (`main_version`) only
/// the main ones. Bulk opinions hold neither field. What they amount to on the 893 stored pages of clusters with
/// several opinions (2026-11-07): the separate opinions by id, a combined record beside them never printed; of
/// combined records alone, one: the last by id with html_with_citations, the others older copies (43 of 43, their whole
/// text on the page). That holds on 890 of the 893; the other 3 print an order their ids do not give.
fn printed_opinions(cluster: &CourtListenerCluster) -> Vec<&CourtListenerOpinion> {
    let mut opinions = cluster.opinions.iter().collect::<Vec<_>>();
    opinions.sort_by_key(|opinion| opinion.id);
    let separate = opinions.iter().copied().filter(|opinion| opinion.kind != "010combined").collect::<Vec<_>>();
    if !separate.is_empty() {
        return separate;
    }
    let last = opinions.iter().copied().rfind(|opinion| !opinion.html_with_citations.is_empty()).or(opinions.first().copied());
    last.into_iter().collect()
}

/// Django's default DATE_FORMAT, "N j, Y" ("April 1, 1759", "Sept. 9, 1921").
fn date(value: &str) -> String {
    let mut parts = value.split('-').map(|part| part.parse::<u32>().ok());
    let (Some(Some(year)), Some(Some(month)), Some(Some(day))) = (parts.next(), parts.next(), parts.next()) else { return value.to_owned() };
    const MONTHS: [&str; 12] = ["Jan.", "Feb.", "March", "April", "May", "June", "July", "Aug.", "Sept.", "Oct.", "Nov.", "Dec."];
    format!("{} {day}, {year}", MONTHS.get(month as usize - 1).copied().unwrap_or(""))
}

/// What the page prints above the opinion tab (opinions.html, base.html): the site's menu, the sidebar's jump links
/// and notices, then the caption. Bulk clusters do not hold the court's name, the citations, the docket number, the
/// tab counts or whether a PDF is offered: those words are left out.
fn page_head(cluster: &CourtListenerCluster) -> String {
    let mut lines: Vec<String> = ["About", "Sign in / Register", "Case Law", "RECAP Archive", "Oral Arguments", "Judges",
        "Financial Disclosures", "Join FLP", "Jump To", "Top", "Caption"].map(str::to_owned).to_vec();
    if !cluster.headmatter.is_empty() {
        lines.push("Headmatter".into());
    } else {
        for (value, name) in [(&cluster.correction, "Correction"), (&cluster.attorneys, "Attorneys"), (&cluster.headnotes, "Headnotes"),
            (&cluster.syllabus, "Syllabus"), (&cluster.summary, "Summary"), (&cluster.history, "History"), (&cluster.disposition, "Disposition")] {
            if !value.is_empty() {
                lines.push(name.into());
            }
        }
    }
    // "count > 1" counts every opinion of the cluster, printed or not.
    let several = cluster.opinions.len() > 1;
    let printed = printed_opinions(cluster);
    for opinion in &printed {
        let kind = type_display(&opinion.kind);
        let kind = if several && kind == "Concurrence Opinion" { "Concurrence" } else { kind };
        lines.push(if opinion.author_str.is_empty() { kind.to_owned() } else { format!("{kind} by {}", opinion.author_str) });
    }
    // `cluster.sub_opinions.all.0`: the first opinion as the database returns them, by id here.
    if cluster.opinions.iter().min_by_key(|opinion| opinion.id).is_some_and(|opinion| opinion.extracted_by_ocr) || cluster.source.contains('U') {
        lines.push("The text of this document was obtained by analyzing a scanned document and may have typos.".into());
    }
    lines.extend(["Support FLP", "CourtListener is a project of Free Law Project, a federally-recognized 501(c)(3) non-profit. \
        Members help support our work and get special access to features.", "Please become a member today.", "Join Free.law Now"]
        .map(str::to_owned));
    if !cluster.date_filed.is_empty() {
        lines.push(date(&cluster.date_filed));
    }
    lines.extend(["Get Citation Alerts", "Toggle Dropdown"].map(str::to_owned));
    let name = [&cluster.case_name, &cluster.case_name_full, &cluster.case_name_short].into_iter().find(|name| !name.is_empty());
    if let Some(name) = name {
        lines.push(name.clone());
    }
    // The court's name, the citations and the docket number follow; bulk clusters hold none of them.
    lines.extend(["Citations:", "Docket Number:"].map(str::to_owned));
    // models.py PRECEDENTIAL_STATUS.NAMES; shown unless "Precedential".
    let status = match cluster.precedential_status.as_str() {
        "Published" => "Precedential", "Unpublished" => "Non-Precedential", "Separate" => "Separate Opinion",
        "Relating-to" => "Relating-to orders", "Unknown" => "Unknown Status", "" => "Unknown", other => other,
    };
    if status != "Precedential" {
        lines.push(format!("Precedential Status: {status}"));
    }
    for (label, value) in [("Judges", &cluster.judges), ("Nature of Suit", &cluster.nature_of_suit), ("Posture", &cluster.posture),
        ("Other Dates", &cluster.other_dates), ("Disposition", &cluster.disposition)] {
        if !value.is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
    // The tabs (includes/opinion_tabs.html), their counts left out.
    lines.extend(["Opinion", "Authorities", "Cited By", "Summaries", "Similar Cases", "PDF"].map(str::to_owned));
    lines.join("\n")
}

/// The page CourtListener publishes for `cluster`, as the finder reads it.
pub(crate) fn print_page(cluster: &CourtListenerCluster) -> PrintedPage {
    let mut printer = Printer::default();
    printer.words(&page_head(cluster), true);
    printer.end();
    let article = printer.text.len();
    let mut dom = Dom::parse(&opinion_tab(cluster));
    apply_page_script(&mut dom);
    printer.read(&dom, 0, false, true);
    printer.end();
    let Printer { text, mut marks, mut anchors } = printer;
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for anchor in &anchors {
        *seen.entry(anchor.id.clone()).or_default() += 1;
    }
    for anchor in &mut anchors {
        anchor.unique = seen[&anchor.id] == 1;
    }
    marks.sort_unstable();
    PrintedPage { text, marks, anchors, article }
}
