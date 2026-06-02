set shell := ["bash", "-uc"]

ref_dir := "ref"
repos   := "oc-go-cc=https://github.com/samueltuyizere/oc-go-cc llm-proxy=https://github.com/llm-proxy/llm-proxy litellm=https://github.com/BerriAI/litellm llm-api-key-proxy=https://github.com/Mirrowel/LLM-API-Key-Proxy"

[doc("Show available recipes")]
default:
    @just --list

[doc("Reference repo commands. Usage: just ref <check|pull>")]
ref subcommand:
    @case "{{subcommand}}" in \
        check) just _ref-check ;; \
        pull)  just _ref-pull ;; \
        *) echo "usage: just ref {check|pull}" >&2; exit 1 ;; \
    esac

[group('ref')]
[doc("Fetch each reference repo and report whether it is up to date with origin")]
_ref-check:
    #!/usr/bin/env bash
    set -uo pipefail
    any=0
    for pair in {{repos}}; do
        name="${pair%%=*}"
        dest="{{ref_dir}}/${name}"
        if [ ! -d "${dest}" ]; then
            printf '%-18s MISSING  (no clone)\n' "${name}"
            any=1
            continue
        fi
        if ! git -C "${dest}" fetch --depth=1 origin 2>/dev/null; then
            printf '%-18s UNREACHABLE\n' "${name}"
            any=1
            continue
        fi
        local="$(git -C "${dest}" rev-parse HEAD)"
        remote="$(git -C "${dest}" rev-parse FETCH_HEAD)"
        if [ "${local}" = "${remote}" ]; then
            printf '%-18s up to date  %s\n' "${name}" "${local:0:12}"
        else
            printf '%-18s BEHIND       local=%s  remote=%s\n' "${name}" "${local:0:12}" "${remote:0:12}"
            any=1
        fi
    done
    exit "${any}"

[group('ref')]
[doc("Pull latest for each reference repo, continuing past failures, then rewrite ref/REFS")]
_ref-pull:
    #!/usr/bin/env bash
    set -uo pipefail
    failures=0
    for pair in {{repos}}; do
        name="${pair%%=*}"
        url="${pair#*=}"
        dest="{{ref_dir}}/${name}"
        if [ ! -d "${dest}" ]; then
            echo "cloning ${name} from ${url}"
            if ! git clone --depth=1 "${url}" "${dest}"; then
                printf '  %s clone FAILED\n' "${name}" >&2
                failures=$((failures + 1))
            fi
        else
            echo "pulling ${name}"
            if ! ( git -C "${dest}" fetch --depth=1 origin && git -C "${dest}" reset --hard FETCH_HEAD ); then
                printf '  %s pull FAILED\n' "${name}" >&2
                failures=$((failures + 1))
            fi
        fi
    done
    : > "{{ref_dir}}/REFS"
    for pair in {{repos}}; do
        name="${pair%%=*}"
        dest="{{ref_dir}}/${name}"
        if [ ! -d "${dest}" ]; then
            printf '%-18s %s\n' "${name}" "FAILED" >> "{{ref_dir}}/REFS"
            continue
        fi
        hash="$(git -C "${dest}" rev-parse HEAD)"
        msg="$(git -C "${dest}" log -1 --format='%ci %s')"
        printf '%-18s %s  %s\n' "${name}" "${hash}" "${msg}" >> "{{ref_dir}}/REFS"
    done
    echo "---"
    cat "{{ref_dir}}/REFS"
    if [ "${failures}" -gt 0 ]; then
        echo "${failures} repo(s) failed" >&2
        exit 1
    fi
