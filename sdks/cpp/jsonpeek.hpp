// jsonpeek.hpp — pull a few fields out of a JSON document, for printing.
//
// THIS IS NOT A JSON PARSER. It scans for `"key":` and reads what follows,
// understanding \" and \\ and nothing else. It does not validate, does not
// handle nesting, and does not know that a key inside a string literal is not
// a key. It is here so the examples have no dependencies; a real program links
// nlohmann/json, RapidJSON or simdjson, and patala speaks ordinary JSON so any
// of them works unchanged.
//
// One rule here is NOT a shortcut and should survive into your real code:
// there is no `number()` returning a double. Amounts in patala are integer
// minor units, a double loses every integer above 2^53, and this is money.
// `u64()` REFUSES `1250.0` rather than truncating it.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#ifndef PATALA_JSONPEEK_HPP
#define PATALA_JSONPEEK_HPP

#include <cstdint>
#include <optional>
#include <string>

namespace peek {

// Position just past `"key":`, skipping spaces.
inline std::optional<std::size_t> after_key(const std::string &doc, const std::string &key) {
	const std::string needle = "\"" + key + "\":";
	const std::size_t at = doc.find(needle);
	if (at == std::string::npos) return std::nullopt;
	std::size_t i = at + needle.size();
	while (i < doc.size() && doc[i] == ' ') i++;
	return i;
}

inline std::optional<std::string> str(const std::string &doc, const std::string &key) {
	auto start = after_key(doc, key);
	if (!start || *start >= doc.size() || doc[*start] != '"') return std::nullopt;
	std::string out;
	for (std::size_t i = *start + 1; i < doc.size(); i++) {
		char c = doc[i];
		if (c == '"') return out;
		if (c == '\\' && i + 1 < doc.size()) {
			c = doc[++i];
			if (c == 'n') c = '\n';
			else if (c == 't') c = '\t';
			else if (c == 'r') c = '\r';
		}
		out.push_back(c);
	}
	return std::nullopt; // unterminated
}

inline std::optional<std::uint64_t> u64(const std::string &doc, const std::string &key) {
	auto start = after_key(doc, key);
	if (!start) return std::nullopt;
	std::size_t i = *start;
	if (i >= doc.size() || doc[i] < '0' || doc[i] > '9') return std::nullopt;
	std::uint64_t v = 0;
	while (i < doc.size() && doc[i] >= '0' && doc[i] <= '9') {
		v = v * 10 + static_cast<std::uint64_t>(doc[i] - '0');
		i++;
	}
	// A fractional amount is a defect to notice, not a value to round.
	if (i < doc.size() && (doc[i] == '.' || doc[i] == 'e' || doc[i] == 'E')) return std::nullopt;
	return v;
}

inline std::optional<bool> flag(const std::string &doc, const std::string &key) {
	auto start = after_key(doc, key);
	if (!start) return std::nullopt;
	if (doc.compare(*start, 4, "true") == 0) return true;
	if (doc.compare(*start, 5, "false") == 0) return false;
	return std::nullopt;
}

} // namespace peek

#endif // PATALA_JSONPEEK_HPP
