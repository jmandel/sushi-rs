

Alias: 1950-04-01 = http://aprilfools.com

Profile: 2000-01
Parent: Observation
* obeys 2000-10-31T00:01:02.000+00:00

Profile: 2001
Parent: 2000-01

Extension: 2000-10
* value[x] from 2000-10-31

ValueSet: 2000-10-31
* 2000-10-31T00#2001-01
* 1950-04-01#2001-01-01

CodeSystem: 2000-10-31T00
* #2001-01 "Jan 2001" "January 2001"

Logical: 2000-10-31T00:01
* 1999-12-31 0..1 dateTime "Party" "Party like it's 1999"

Resource: 2000-10-31T00:01:02
Parent: DomainResource
* contained only 2000

Instance: 2000-10-31T00:01:02.000
InstanceOf: Observation
* insert 12:30
* code = #123

Instance: 1800-02-28
InstanceOf: 2000-10-31T00:01
* 1999-12-31 = 2000-01-01

Invariant: 2000-10-31T00:01:02.000+00:00
Description: "It shall not defy the laws of physics nor the laws of men"
Severity: #error

Mapping: 12
Source: 2000-10-31T00:01
Target: "http://unknown.org/mystery"
* 1999-12-31 -> "Patient"

RuleSet: 12:30
* status = #final
