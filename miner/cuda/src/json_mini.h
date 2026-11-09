// Minimal JSON reader for the flat ZPU line protocol (objects, arrays,
// strings, numbers, true/false/null). Inputs come from the local zpu-cli.
#pragma once
#include <map>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace zj {

struct Value {
    enum Kind { Null, Bool, Num, Str, Arr, Obj } kind = Null;
    bool b = false;
    double num = 0;
    std::string str;
    std::vector<Value> arr;
    std::map<std::string, Value> obj;
    const Value& operator[](const std::string& k) const {
        static Value null;
        auto it = obj.find(k);
        return it == obj.end() ? null : it->second;
    }
    std::string s() const { return kind == Str ? str : kind == Num ? std::to_string((long long)num) : ""; }
    long long i() const { return kind == Num ? (long long)num : kind == Str ? std::stoll(str) : 0; }
    unsigned long long u64() const { return kind == Str ? std::stoull(str) : (unsigned long long)num; }
};

struct Parser {
    const std::string& t;
    size_t p = 0;
    explicit Parser(const std::string& s) : t(s) {}
    void ws() { while (p < t.size() && (t[p] == ' ' || t[p] == '\n' || t[p] == '\r' || t[p] == '\t')) p++; }
    Value parse() {
        ws();
        if (p >= t.size()) throw std::runtime_error("eof");
        char c = t[p];
        Value v;
        if (c == '{') {
            v.kind = Value::Obj; p++; ws();
            if (t[p] == '}') { p++; return v; }
            while (true) {
                ws(); Value k = parse(); ws();
                if (t[p++] != ':') throw std::runtime_error("expected :");
                v.obj[k.str] = parse(); ws();
                if (t[p] == ',') { p++; continue; }
                if (t[p] == '}') { p++; return v; }
                throw std::runtime_error("bad object");
            }
        } else if (c == '[') {
            v.kind = Value::Arr; p++; ws();
            if (t[p] == ']') { p++; return v; }
            while (true) {
                v.arr.push_back(parse()); ws();
                if (t[p] == ',') { p++; continue; }
                if (t[p] == ']') { p++; return v; }
                throw std::runtime_error("bad array");
            }
        } else if (c == '"') {
            v.kind = Value::Str; p++;
            while (t[p] != '"') {
                if (t[p] == '\\') { p++; }
                v.str += t[p++];
            }
            p++;
        } else if (t.compare(p, 4, "true") == 0) { v.kind = Value::Bool; v.b = true; p += 4; }
        else if (t.compare(p, 5, "false") == 0) { v.kind = Value::Bool; p += 5; }
        else if (t.compare(p, 4, "null") == 0) { p += 4; }
        else {
            size_t q = p;
            while (q < t.size() && (isdigit((unsigned char)t[q]) || t[q] == '-' || t[q] == '+' || t[q] == '.' || t[q] == 'e' || t[q] == 'E')) q++;
            v.kind = Value::Num; v.num = std::stod(t.substr(p, q - p)); p = q;
        }
        return v;
    }
};

inline Value parse(const std::string& s) { Parser ps(s); return ps.parse(); }

}  // namespace zj
