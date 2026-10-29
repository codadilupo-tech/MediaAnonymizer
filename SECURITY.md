# Security and privacy policy

MediaAnonymizer is used to protect people's privacy. A bug that leaves
identifying information in an output file (metadata not removed, a face not
covered in a reproducible case, an embedded preview left behind) is treated as
a **security issue**.

## Reporting

Please **do not open a public issue** for vulnerabilities or privacy leaks.
Use GitHub's private vulnerability reporting
(*Security* tab → *Report a vulnerability*) and include:

* the version (`mediaanon --version`) and the command line or GUI settings;
* the smallest file that reproduces the problem, **with all real personal data
  replaced** (or a description of how to build such a file);
* what remains in the output (e.g. the `mediaanon inspect` output).

We aim to acknowledge reports within 7 days and to release a fix as quickly as
the severity requires. Reporters are credited unless they prefer otherwise.

## Scope

In scope: metadata that survives the selected policy, previews/secondary
images that survive face redaction, crashes or memory-safety issues on crafted
input files, unsafe handling of temporary files.

Out of scope: faces the detector does not recognise in general (please open a
regular issue with a public-domain example), identification through non-face
features, file-system metadata.
