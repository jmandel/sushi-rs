
        Resource: TestResource
        * isValid 1..1 MS boolean "is it valid?"
        * stuff 0..* string "just stuff" "a list of some stuff"
        * address 1..* Address "Just an address"
        * extraThing 0..3 contentReference http://example.org/StructureDefinition/Thing#Thing.extra "extra thing"
        