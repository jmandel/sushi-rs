RuleSet: Inner(x)
* ^title = "{x}"

RuleSet: Outer(y)
* insert Inner({y})

Profile: PN
Parent: Observation
* insert Outer(hello)
