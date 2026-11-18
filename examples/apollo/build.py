"""Builds the graph in the README's screenshots: the Apollo missions, their crews, rockets and
landing sites, read from the lead sections of 17 Wikipedia articles. Each article is one write,
and every node and link cites the verbatim sentence fragment it comes from.

    python build.py [server URL] [API key]

The server defaults to http://127.0.0.1:7420 and needs a graph named "apollo":

    curl -s -X POST localhost:7420/admin/graphs -d '{"name": "apollo"}'

Running it again changes nothing (writes are idempotent). Then open
http://127.0.0.1:7420/admin/ui/graph?graph=apollo
"""
import json, re, sys, urllib.parse, urllib.request

SERVER = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:7420").rstrip("/")
KEY = sys.argv[2] if len(sys.argv) > 2 else None
BASE = f"{SERVER}/v1/graphs/apollo/write"

TYPES = {}  # name -> type


def T(type_, *names):
    for n in names:
        TYPES[n] = type_


T("Program", "Apollo program", "Project Mercury", "Project Gemini", "Skylab",
  "Apollo–Soyuz Test Project", "Space Race")
T("Mission", *[f"Apollo {n}" for n in (1, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17)])
T("Astronaut", "Gus Grissom", "Ed White", "Roger B. Chaffee", "Walter M. Schirra", "Donn F. Eisele",
  "R. Walter Cunningham", "Frank Borman", "Jim Lovell", "William Anders", "James McDivitt",
  "David Scott", "Rusty Schweickart", "Thomas Stafford", "John Young", "Gene Cernan",
  "Neil Armstrong", "Michael Collins", "Buzz Aldrin", "Pete Conrad", "Alan L. Bean",
  "Richard F. Gordon", "Jack Swigert", "Fred Haise", "Ken Mattingly", "Alan Shepard",
  "Stuart Roosa", "Edgar Mitchell", "James Irwin", "Alfred Worden", "Charles Duke",
  "Harrison Schmitt", "Ronald Evans", "Joe Engle")
T("Person", "John F. Kennedy", "Dwight D. Eisenhower", "Richard Nixon", "Walter Mondale",
  "James E. Webb", "Wernher von Braun", "George Mueller", "Kurt Debus", "Arthur Rudolph",
  "Charles Schulz")
T("Organization", "NASA", "North American Aviation", "Grumman", "Marshall Space Flight Center",
  "United States Congress", "Soviet Union")
T("Rocket", "Saturn V", "Saturn IB", "Saturn I", "S-IVB")
T("Spacecraft", "Apollo command and service module", "Apollo Lunar Module", "Lunar Roving Vehicle",
  "Columbia", "Eagle", "Charlie Brown", "Snoopy", "Surveyor 3")
T("LaunchSite", "Kennedy Space Center", "Cape Kennedy Air Force Station", "Launch Complex 34",
  "Launch Complex 39")
T("LunarSite", "Sea of Tranquility", "Ocean of Storms", "Fra Mauro formation", "Hadley Rille",
  "Descartes Highlands", "Taurus–Littrow")
T("Place", "Pacific Ocean", "Atlantic Ocean", "Huntsville, Alabama")
T("Event", "Apollo 1 fire", "Apollo 13 oxygen tank explosion", "Earthrise")

ALIASES = {"Buzz Aldrin": ["Edwin \"Buzz\" Aldrin"], "Pete Conrad": ["Charles \"Pete\" Conrad"],
           "Sea of Tranquility": ["Mare Tranquillitatis"], "Apollo Lunar Module": ["LM", "Lunar Excursion Module"],
           "Apollo command and service module": ["CSM"], "NASA": ["National Aeronautics and Space Administration"]}


def key(name):
    return TYPES[name].lower() + ":" + re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


# page -> [(fragment, [(from, LINK, to, props?)])]
PAGES = {
 "Apollo_program": [
  ("was the United States human spaceflight program led by NASA", [("NASA", "LED", "Apollo program")]),
  ("conceived in 1960 in the Dwight D. Eisenhower presidency during Project Mercury and executed after Project Gemini",
   [("Dwight D. Eisenhower", "CONCEIVED", "Apollo program"), ("Apollo program", "FOLLOWED", "Project Gemini"),
    ("Project Gemini", "FOLLOWED", "Project Mercury")]),
  ("dedicated to President John F. Kennedy's national goal", [("John F. Kennedy", "SET_GOAL_FOR", "Apollo program")]),
  ("It encountered a major setback in 1967 when the Apollo 1 cabin fire killed the entire crew",
   [("Apollo 1 fire", "SET_BACK", "Apollo program")]),
  ("which consisted of Skylab, a space station", [("Skylab", "REUSED_HARDWARE_OF", "Apollo program")]),
  ("the Apollo–Soyuz Test Project, a joint United States-Soviet Union low Earth orbit mission in 1975",
   [("Apollo–Soyuz Test Project", "REUSED_HARDWARE_OF", "Apollo program"),
    ("Soviet Union", "TOOK_PART_IN", "Apollo–Soyuz Test Project")]),
 ],
 "Apollo_1": [
  ("Launch Complex 34 on January 27, 1967 killed all three crew members",
   [("Apollo 1 fire", "HAPPENED_AT", "Launch Complex 34"), ("Apollo 1", "ENDED_BY", "Apollo 1 fire"),
    ("Launch Complex 34", "PART_OF", "Cape Kennedy Air Force Station")]),
  ("command pilot Gus Grissom, senior pilot Ed White, and pilot Roger B. Chaffee",
   [("Gus Grissom", "CREW_OF", "Apollo 1", {"role": "Command pilot"}),
    ("Ed White", "CREW_OF", "Apollo 1", {"role": "Senior pilot"}),
    ("Roger B. Chaffee", "CREW_OF", "Apollo 1", {"role": "Pilot"})]),
  ("Senator Walter Mondale publicly revealed a NASA internal document",
   [("Walter Mondale", "INVESTIGATED", "Apollo 1 fire"), ("Walter Mondale", "MEMBER_OF", "United States Congress")]),
  ("prime Apollo contractor North American Aviation", [("North American Aviation", "CONTRACTOR_OF", "Apollo program")]),
  ("This disclosure embarrassed NASA administrator James E. Webb", [("James E. Webb", "ADMINISTERED", "NASA")]),
  ("The Saturn IB launch vehicle for Apollo 1, AS-204, was used for the first LM test flight, Apollo 5",
   [("Apollo 5", "LAUNCHED_ON", "Saturn IB"), ("Apollo 5", "TESTED", "Apollo Lunar Module")]),
 ],
 "Apollo_7": [
  ("was the first crewed flight in NASA's Apollo program", [("Apollo 7", "PART_OF", "Apollo program")]),
  ("commanded by Walter M. Schirra, with Command Module Pilot Donn F. Eisele and Lunar Module pilot R. Walter Cunningham",
   [("Walter M. Schirra", "CREW_OF", "Apollo 7", {"role": "Commander"}),
    ("Donn F. Eisele", "CREW_OF", "Apollo 7", {"role": "Command Module Pilot"}),
    ("R. Walter Cunningham", "CREW_OF", "Apollo 7", {"role": "Lunar Module Pilot"})]),
  ("from Cape Kennedy Air Force Station, Florida, and splashed down in the Atlantic Ocean",
   [("Apollo 7", "LAUNCHED_FROM", "Cape Kennedy Air Force Station"), ("Apollo 7", "SPLASHED_DOWN_IN", "Atlantic Ocean")]),
  ("Apollo 7 fulfilled Apollo 1's mission of testing the CSM in low Earth orbit",
   [("Apollo 7", "FULFILLED", "Apollo 1"), ("Apollo 7", "TESTED", "Apollo command and service module")]),
 ],
 "Apollo_8": [
  ("Frank Borman, Jim Lovell, and William Anders",
   [("Frank Borman", "CREW_OF", "Apollo 8", {"role": "Commander"}),
    ("Jim Lovell", "CREW_OF", "Apollo 8", {"role": "Command Module Pilot"}),
    ("William Anders", "CREW_OF", "Apollo 8", {"role": "Lunar Module Pilot"})]),
  ("the first humans to see and photograph the far side of the Moon and an Earthrise", [("Apollo 8", "PHOTOGRAPHED", "Earthrise")]),
  ("the first crewed launch of the Saturn V rocket", [("Apollo 8", "LAUNCHED_ON", "Saturn V")]),
  ("It was the first human spaceflight from the Kennedy Space Center, adjacent to Cape Kennedy Air Force Station",
   [("Apollo 8", "LAUNCHED_FROM", "Kennedy Space Center"), ("Apollo 8", "PART_OF", "Apollo program")]),
  ("splashed down in the northern Pacific Ocean", [("Apollo 8", "SPLASHED_DOWN_IN", "Pacific Ocean")]),
 ],
 "Apollo_9": [
  ("Commander James McDivitt, Command Module Pilot David Scott, and Lunar Module Pilot Rusty Schweickart",
   [("James McDivitt", "CREW_OF", "Apollo 9", {"role": "Commander"}),
    ("David Scott", "CREW_OF", "Apollo 9", {"role": "Command Module Pilot"}),
    ("Rusty Schweickart", "CREW_OF", "Apollo 9", {"role": "Lunar Module Pilot"})]),
  ("launched via a Saturn V rocket, and was the first flight of the full Apollo spacecraft",
   [("Apollo 9", "LAUNCHED_ON", "Saturn V"), ("Apollo 9", "TESTED", "Apollo Lunar Module"),
    ("Apollo 9", "PART_OF", "Apollo program")]),
 ],
 "Apollo_10": [
  ("described it as a \"dress rehearsal\" for the first Moon landing", [("Apollo 10", "REHEARSED", "Apollo 11"),
                                                                       ("Apollo 10", "PART_OF", "Apollo program")]),
  ("astronaut John Young remained in the Command and Service Module (CSM) while astronauts Thomas Stafford and Gene Cernan flew",
   [("John Young", "CREW_OF", "Apollo 10", {"role": "Command Module Pilot"}),
    ("Thomas Stafford", "CREW_OF", "Apollo 10", {"role": "Commander"}),
    ("Gene Cernan", "CREW_OF", "Apollo 10", {"role": "Lunar Module Pilot"})]),
  ("photographed Apollo 11's planned landing site in the Sea of Tranquility", [("Apollo 10", "SURVEYED", "Sea of Tranquility")]),
  ("Charlie Brown for the CSM and Snoopy for the LM",
   [("Apollo 10", "FLEW", "Charlie Brown"), ("Apollo 10", "FLEW", "Snoopy"),
    ("Charlie Brown", "INSTANCE_OF", "Apollo command and service module"),
    ("Snoopy", "INSTANCE_OF", "Apollo Lunar Module")]),
  ("Peanuts creator Charles Schulz also drew mission-related artwork for NASA", [("Charles Schulz", "DREW_FOR", "Apollo 10")]),
 ],
 "Apollo_11": [
  ("Commander Neil Armstrong, Command Module Pilot Michael Collins, and Lunar Module Pilot Edwin \"Buzz\" Aldrin",
   [("Neil Armstrong", "CREW_OF", "Apollo 11", {"role": "Commander"}),
    ("Michael Collins", "CREW_OF", "Apollo 11", {"role": "Command Module Pilot"}),
    ("Buzz Aldrin", "CREW_OF", "Apollo 11", {"role": "Lunar Module Pilot"})]),
  ("Launched atop a Saturn V rocket from Kennedy Space Center in Florida",
   [("Apollo 11", "LAUNCHED_ON", "Saturn V"), ("Apollo 11", "LAUNCHED_FROM", "Kennedy Space Center"),
    ("Apollo 11", "PART_OF", "Apollo program")]),
  ("aboard the LM Eagle, landing in the Sea of Tranquility",
   [("Apollo 11", "LANDED_AT", "Sea of Tranquility"), ("Apollo 11", "FLEW", "Eagle"),
    ("Eagle", "INSTANCE_OF", "Apollo Lunar Module")]),
  ("Collins remained in lunar orbit aboard the CM Columbia",
   [("Apollo 11", "FLEW", "Columbia"), ("Columbia", "INSTANCE_OF", "Apollo command and service module")]),
  ("speaking with President Richard Nixon", [("Richard Nixon", "SPOKE_WITH", "Apollo 11")]),
  ("splashing down in the Pacific Ocean", [("Apollo 11", "SPLASHED_DOWN_IN", "Pacific Ocean")]),
  ("Apollo 11 was the culmination of the Space Race", [("Apollo 11", "PART_OF", "Space Race"),
                                                       ("Soviet Union", "TOOK_PART_IN", "Space Race")]),
 ],
 "Apollo_12": [
  ("landing on the part of the Moon called the Ocean of Storms",
   [("Apollo 12", "LANDED_AT", "Ocean of Storms"), ("Apollo 12", "LAUNCHED_FROM", "Kennedy Space Center"),
    ("Apollo 12", "PART_OF", "Apollo program")]),
  ("Commander Charles \"Pete\" Conrad and Lunar Module Pilot Alan L. Bean",
   [("Pete Conrad", "CREW_OF", "Apollo 12", {"role": "Commander"}),
    ("Alan L. Bean", "CREW_OF", "Apollo 12", {"role": "Lunar Module Pilot"})]),
  ("Command Module Pilot Richard F. Gordon remained in lunar orbit",
   [("Richard F. Gordon", "CREW_OF", "Apollo 12", {"role": "Command Module Pilot"})]),
  ("within walking distance of the Surveyor 3 robotic probe", [("Apollo 12", "VISITED", "Surveyor 3"),
                                                               ("Surveyor 3", "LANDED_AT", "Ocean of Storms")]),
 ],
 "Apollo_13": [
  ("an oxygen tank in the service module (SM) exploded two days into the mission",
   [("Apollo 13", "ABORTED_BY", "Apollo 13 oxygen tank explosion"), ("Apollo 13", "PART_OF", "Apollo program"),
    ("Apollo 13", "LAUNCHED_FROM", "Kennedy Space Center")]),
  ("The mission was commanded by Jim Lovell, with Jack Swigert as command module (CM) pilot and Fred Haise as Lunar Module (LM) pilot",
   [("Jim Lovell", "CREW_OF", "Apollo 13", {"role": "Commander"}),
    ("Jack Swigert", "CREW_OF", "Apollo 13", {"role": "Command Module Pilot"}),
    ("Fred Haise", "CREW_OF", "Apollo 13", {"role": "Lunar Module Pilot"})]),
  ("Swigert was a late replacement for Ken Mattingly", [("Jack Swigert", "REPLACED", "Ken Mattingly")]),
  ("forcing the crew to transfer to the LM as a lifeboat", [("Apollo 13", "USED_AS_LIFEBOAT", "Apollo Lunar Module")]),
  ("splashdown in the South Pacific Ocean", [("Apollo 13", "SPLASHED_DOWN_IN", "Pacific Ocean")]),
 ],
 "Apollo_14": [
  ("Commander Alan Shepard, Command Module Pilot Stuart Roosa, and Lunar Module Pilot Edgar Mitchell",
   [("Alan Shepard", "CREW_OF", "Apollo 14", {"role": "Commander"}),
    ("Stuart Roosa", "CREW_OF", "Apollo 14", {"role": "Command Module Pilot"}),
    ("Edgar Mitchell", "CREW_OF", "Apollo 14", {"role": "Lunar Module Pilot"})]),
  ("in the Fra Mauro formation – originally the target of Apollo 13",
   [("Apollo 14", "LANDED_AT", "Fra Mauro formation"), ("Apollo 13", "TARGETED", "Fra Mauro formation"),
    ("Apollo 14", "PART_OF", "Apollo program")]),
  ("splashed down safely in the Pacific Ocean", [("Apollo 14", "SPLASHED_DOWN_IN", "Pacific Ocean")]),
 ],
 "Apollo_15": [
  ("Apollo 15 saw the first use of the Lunar Roving Vehicle", [("Apollo 15", "USED", "Lunar Roving Vehicle"),
                                                               ("Apollo 15", "PART_OF", "Apollo program")]),
  ("Commander David Scott and Lunar Module Pilot James Irwin landed near Hadley Rille",
   [("David Scott", "CREW_OF", "Apollo 15", {"role": "Commander"}),
    ("James Irwin", "CREW_OF", "Apollo 15", {"role": "Lunar Module Pilot"}),
    ("Apollo 15", "LANDED_AT", "Hadley Rille")]),
  ("Command Module Pilot Alfred Worden orbited the Moon",
   [("Alfred Worden", "CREW_OF", "Apollo 15", {"role": "Command Module Pilot"})]),
 ],
 "Apollo_16": [
  ("The landing and exploration were in the Descartes Highlands", [("Apollo 16", "LANDED_AT", "Descartes Highlands"),
                                                                   ("Apollo 16", "USED", "Lunar Roving Vehicle"),
                                                                   ("Apollo 16", "PART_OF", "Apollo program")]),
  ("Commander John Young, Lunar Module Pilot Charles Duke and Command Module Pilot Ken Mattingly",
   [("John Young", "CREW_OF", "Apollo 16", {"role": "Commander"}),
    ("Charles Duke", "CREW_OF", "Apollo 16", {"role": "Lunar Module Pilot"}),
    ("Ken Mattingly", "CREW_OF", "Apollo 16", {"role": "Command Module Pilot"})]),
  ("Launched from the Kennedy Space Center in Florida on April 16, 1972", [("Apollo 16", "LAUNCHED_FROM", "Kennedy Space Center")]),
 ],
 "Apollo_17": [
  ("was the eleventh and final crewed mission of NASA's Apollo program", [("Apollo 17", "PART_OF", "Apollo program")]),
  ("Commander Gene Cernan and Lunar Module Pilot Harrison Schmitt walked on the Moon, while Command Module Pilot Ronald Evans orbited above",
   [("Gene Cernan", "CREW_OF", "Apollo 17", {"role": "Commander"}),
    ("Harrison Schmitt", "CREW_OF", "Apollo 17", {"role": "Lunar Module Pilot"}),
    ("Ronald Evans", "CREW_OF", "Apollo 17", {"role": "Command Module Pilot"})]),
  ("he was selected in place of Joe Engle", [("Harrison Schmitt", "REPLACED", "Joe Engle")]),
  ("They therefore selected Taurus–Littrow", [("Apollo 17", "LANDED_AT", "Taurus–Littrow")]),
  ("the use of the third Lunar Roving Vehicle", [("Apollo 17", "USED", "Lunar Roving Vehicle")]),
 ],
 "Saturn_V": [
  ("developed by NASA under the Apollo program", [("NASA", "DEVELOPED", "Saturn V"), ("Saturn V", "BUILT_FOR", "Apollo program")]),
  ("all from Kennedy Space Center Launch Complex 39",
   [("Saturn V", "LAUNCHED_FROM", "Launch Complex 39"), ("Launch Complex 39", "PART_OF", "Kennedy Space Center")]),
  ("Its final launch was Skylab", [("Skylab", "LAUNCHED_ON", "Saturn V")]),
  ("which included the S-IVB third stage", [("S-IVB", "STAGE_OF", "Saturn V")]),
  ("designed at the Marshall Space Flight Center (MSFC) in Huntsville, Alabama",
   [("Saturn V", "DESIGNED_AT", "Marshall Space Flight Center"), ("Marshall Space Flight Center", "LOCATED_IN", "Huntsville, Alabama"),
    ("Marshall Space Flight Center", "PART_OF", "NASA")]),
  ("The program was managed by American George Mueller", [("George Mueller", "MANAGED", "Saturn V")]),
  ("most notably Wernher von Braun, as well as Kurt Debus and Arthur Rudolph",
   [("Wernher von Braun", "DESIGNED", "Saturn V"), ("Kurt Debus", "DESIGNED", "Saturn V"), ("Arthur Rudolph", "DESIGNED", "Saturn V")]),
 ],
 "Saturn_IB": [
  ("It uprated the Saturn I by replacing the S-IV second stage", [("Saturn IB", "UPRATED", "Saturn I")]),
  ("By sharing the S-IVB upper stage, the Saturn IB and Saturn V",
   [("S-IVB", "STAGE_OF", "Saturn IB"), ("Saturn IB", "BUILT_FOR", "Apollo program")]),
  ("the first crewed CSM orbital mission (first planned as Apollo 1, later flown as Apollo 7)",
   [("Apollo 7", "LAUNCHED_ON", "Saturn IB"), ("Apollo 1", "LAUNCHED_ON", "Saturn IB")]),
 ],
 "Apollo_command_and_service_module": [
  ("The CSM was developed and built for NASA by North American Aviation",
   [("North American Aviation", "BUILT", "Apollo command and service module"),
    ("Apollo command and service module", "BUILT_FOR", "Apollo program")]),
  ("carried a crew of three astronauts and the second Apollo spacecraft, the Apollo Lunar Module",
   [("Apollo command and service module", "CARRIED", "Apollo Lunar Module")]),
 ],
 "Apollo_Lunar_Module": [
  ("Overseen by Grumman", [("Grumman", "BUILT", "Apollo Lunar Module"),
                           ("Apollo Lunar Module", "BUILT_FOR", "Apollo program")]),
 ],
}


def lead_sections(pages):
    """The plain-text lead section of each Wikipedia page, in one request."""
    query = urllib.parse.urlencode({"action": "query", "prop": "extracts", "exintro": 1, "explaintext": 1,
                                    "exlimit": "max", "format": "json", "redirects": 1, "titles": "|".join(pages)})
    req = urllib.request.Request(f"https://en.wikipedia.org/w/api.php?{query}",
                                 headers={"user-agent": "mygraph-apollo-example/1.0"})
    with urllib.request.urlopen(req) as r:
        found = json.load(r)["query"]["pages"].values()
    return {p["title"].replace(" ", "_"): p["extract"] for p in found}


def post(body):
    headers = {"content-type": "application/json"}
    if KEY:
        headers["authorization"] = f"Bearer {KEY}"
    req = urllib.request.Request(BASE, json.dumps(body).encode(), headers)
    try:
        with urllib.request.urlopen(req) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        sys.exit(f"{e.code}: {e.read().decode()}")


TEXTS = lead_sections(PAGES)
for page, facts in PAGES.items():
    text = TEXTS[page]
    title = page.replace("_", " ")
    evidence = [{"ref": "@page", "kind": "url", "uri": f"https://en.wikipedia.org/wiki/{page}",
                 "title": f"{title} – Wikipedia", "publisher": "Wikipedia", "license": "CC BY-SA 4.0"}]
    nodes, links, seen = [], [], {}
    for i, (frag, rels) in enumerate(facts):
        if frag not in text:
            sys.exit(f"{page}: the article no longer says {frag!r}; update this fact")
        ev = f"@f{i}"
        evidence.append({"ref": ev, "kind": "text", "content": frag, "parent": "@page"})
        for rel in rels:
            a, ltype, b, *props = rel
            for n in (a, b):
                if n not in seen:
                    seen[n] = f"@n{len(seen)}"
                    node = {"ref": seen[n], "key": key(n), "type": TYPES[n], "name": n, "cite": [ev]}
                    if n in ALIASES:
                        node["aliases"] = ALIASES[n]
                    nodes.append(node)
            link = {"type": ltype, "from": seen[a], "to": seen[b], "cite": [ev]}
            if props:
                link["props"] = props[0]
            links.append(link)
    r = post({"evidence": evidence, "nodes": nodes, "links": links})
    print(page, r.get("counts"))
