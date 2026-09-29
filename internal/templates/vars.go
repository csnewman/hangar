package templates

import (
	"fmt"
	"regexp"
	"slices"
	"sort"
	"strings"
)

// A template's repositories and editor folder may name what is known of an
// environment only once it is made: {name} and the rest of Vars, and each
// named group of the template's name pattern, such as {ticket} from
// (?P<ticket>[A-Z]+-[0-9]+). :lower or :upper after a variable changes its
// case: {ticket:lower}.

// Vars are what is known of an environment when its template's spec is made
// its own.
type Vars struct {
	Name     string
	Owner    string
	ShortID  string
	Template string
}

// The variables every template may use.
const (
	VarName     = "name"
	VarOwner    = "owner"
	VarShortID  = "short_id"
	VarTemplate = "template"
)

var builtins = []string{VarName, VarOwner, VarShortID, VarTemplate}

// placeholder is {variable} or {variable:modifier}.
var placeholder = regexp.MustCompile(`\{([A-Za-z_][A-Za-z0-9_]*)(?::([A-Za-z]+))?\}`)

// strayBrace is a brace left once the placeholders are taken out, which is
// a mistyped one: {name, name}, {na me}.
var strayBrace = regexp.MustCompile(`[{}]`)

var modifiers = map[string]func(string) string{
	"lower": strings.ToLower,
	"upper": strings.ToUpper,
}

// patternGroups are the named groups of a template's name pattern, which it
// may use as variables. A pattern that does not compile has none.
func patternGroups(pattern string) []string {
	if pattern == "" {
		return nil
	}
	re, err := namePattern(pattern)
	if err != nil {
		return nil
	}
	var out []string
	for _, g := range re.SubexpNames() {
		if g != "" {
			out = append(out, g)
		}
	}
	return out
}

// Variables are the names a template may use: the built-in ones and its
// name pattern's groups.
func Variables(pattern string) []string {
	return append(slices.Clone(builtins), patternGroups(pattern)...)
}

// values are each variable's value for an environment of a template: the
// groups a name pattern does not match, being optional, are empty.
func values(pattern string, v Vars) map[string]string {
	vals := map[string]string{VarName: v.Name, VarOwner: v.Owner, VarShortID: v.ShortID, VarTemplate: v.Template}
	if pattern == "" {
		return vals
	}
	re, err := namePattern(pattern)
	if err != nil {
		return vals
	}
	m := re.FindStringSubmatch(v.Name)
	for i, g := range re.SubexpNames() {
		if g == "" {
			continue
		}
		vals[g] = ""
		if m != nil {
			vals[g] = m[i]
		}
	}
	return vals
}

// expand fills vals into s. Every placeholder in it must name one of them,
// with a modifier there is.
func expand(s string, vals map[string]string) (string, error) {
	var bad error
	out := placeholder.ReplaceAllStringFunc(s, func(p string) string {
		m := placeholder.FindStringSubmatch(p)
		val, ok := vals[m[1]]
		if !ok {
			if bad == nil {
				bad = fmt.Errorf("{%s} is not a variable: there are %s", m[1], list(vals))
			}
			return p
		}
		if m[2] != "" {
			f, ok := modifiers[m[2]]
			if !ok {
				if bad == nil {
					bad = fmt.Errorf("%q in %s is not :lower or :upper", m[2], p)
				}
				return p
			}
			val = f(val)
		}
		return val
	})
	if bad != nil {
		return "", bad
	}
	if strayBrace.MatchString(placeholder.ReplaceAllString(s, "")) {
		return "", fmt.Errorf("%q has a brace that is not part of a {variable}", s)
	}
	return out, nil
}

func list(vals map[string]string) string {
	names := make([]string, 0, len(vals))
	for n := range vals {
		names = append(names, "{"+n+"}")
	}
	sort.Strings(names)
	return strings.Join(names, ", ")
}

// standIns are values for checking a template's placeholders when it is
// saved, before there is an environment: each variable's own name, which
// makes a plausible branch or path.
func standIns(pattern string) map[string]string {
	vals := map[string]string{}
	for _, n := range Variables(pattern) {
		vals[n] = n
	}
	return vals
}
