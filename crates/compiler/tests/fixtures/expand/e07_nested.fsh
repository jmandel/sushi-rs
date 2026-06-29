RuleSet: Inner
* ^title = "T"

RuleSet: Outer
* ^status = #active
* insert Inner

Profile: P7
Parent: Observation
* insert Outer
