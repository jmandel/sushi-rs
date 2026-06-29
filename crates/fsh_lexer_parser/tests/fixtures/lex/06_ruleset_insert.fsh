RuleSet: Common
* status = #active

RuleSet: Param(a, b)
* version = "{a}"
* title = "{b}"

Profile: P
Parent: Patient
* insert Common
* insert Param(1.0, [[Hello, World]])
